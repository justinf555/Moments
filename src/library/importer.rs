use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::Semaphore;
use tracing::{debug, info, instrument, warn};

/// Maximum concurrent thumbnail generation tasks during import.
/// Uses half of available cores (minimum 2), same logic as the UI decode pool.
fn max_thumbnail_workers() -> usize {
    (std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        / 2)
    .max(2)
}

use super::config::LocalStorageMode;
use super::db::Database;
use super::error::LibraryError;
use super::exif::extract_exif;
use super::format::FormatRegistry;
use super::import::{ImportSummary, SkipReason};
use super::media::{LibraryMedia, MediaId, MediaMetadataRecord, MediaRecord, MediaType};
use super::thumbnailer::ThumbnailJob;
use crate::app_event::AppEvent;
use crate::event_bus::EventSender;

/// Drives a single import run for the local backend.
///
/// [`ImportJob::run`] is **async** and must be spawned on the Tokio runtime
/// via the handle stored on `LocalLibrary`. Results are communicated back
/// through the [`EventSender`] so the GTK layer receives
/// progress without any extra wiring.
pub struct ImportJob {
    /// Root `originals/` directory inside the bundle.
    originals_dir: PathBuf,
    /// Root `thumbnails/` directory inside the bundle.
    thumbnails_dir: PathBuf,
    /// Open database — used for hash-based duplicate detection and thumbnail tracking.
    db: Database,
    /// Shared event sender for the lifetime of the backend.
    events: EventSender,
    /// Format registry — drives extension filtering and thumbnail decode dispatch.
    formats: Arc<FormatRegistry>,
    /// Limits concurrent thumbnail generation to avoid CPU/memory spikes.
    thumbnail_semaphore: Arc<Semaphore>,
    /// Storage mode — determines whether files are copied (managed) or referenced in place.
    mode: LocalStorageMode,
}

impl ImportJob {
    pub fn new(
        originals_dir: PathBuf,
        thumbnails_dir: PathBuf,
        db: Database,
        events: EventSender,
        formats: Arc<FormatRegistry>,
        mode: LocalStorageMode,
    ) -> Self {
        Self {
            originals_dir,
            thumbnails_dir,
            db,
            events,
            formats,
            thumbnail_semaphore: Arc::new(Semaphore::new(max_thumbnail_workers())),
            mode,
        }
    }

    /// Execute the import asynchronously.
    ///
    /// Must be called from a Tokio async context — spawn via
    /// `tokio_handle.spawn(async move { job.run(sources).await })`.
    #[instrument(skip(self, sources), fields(source_count = sources.len()))]
    pub async fn run(self, sources: Vec<PathBuf>) {
        let start = Instant::now();

        // ── 1. Collect candidate files ────────────────────────────────────────
        let candidates = collect_candidates(sources);
        let total = candidates.len();
        info!(total, "import candidates collected");

        // ── 2. Process each file ──────────────────────────────────────────────
        let mut summary = ImportSummary::default();

        for (idx, path) in candidates.into_iter().enumerate() {
            let current = idx + 1;
            // Receiver may be dropped during shutdown.
            self.events.send(AppEvent::ImportProgress {
                current,
                total,
                imported: summary.imported,
                skipped: summary.skipped_duplicates + summary.skipped_unsupported,
                failed: summary.failed,
            });

            match self.import_one(&path).await {
                Ok(Some(skip)) => {
                    debug!(?path, ?skip, "skipped");
                    match skip {
                        SkipReason::Duplicate => summary.skipped_duplicates += 1,
                        SkipReason::UnsupportedFormat => summary.skipped_unsupported += 1,
                    }
                }
                Ok(None) => {
                    summary.imported += 1;
                }
                Err(e) => {
                    warn!(?path, error = %e, "failed to import file");
                    summary.failed += 1;
                    self.events.send(AppEvent::Error(e.to_string()));
                }
            }
        }

        summary.elapsed_secs = start.elapsed().as_secs_f64();
        info!(
            imported = summary.imported,
            skipped_duplicates = summary.skipped_duplicates,
            skipped_unsupported = summary.skipped_unsupported,
            failed = summary.failed,
            elapsed_secs = summary.elapsed_secs,
            "import complete"
        );

        // Receiver may be dropped during shutdown.
        self.events.send(AppEvent::ImportComplete { summary });
    }

    /// Import a single file.
    ///
    /// Returns `Ok(Some(reason))` if skipped, `Ok(None)` on success, or `Err` on failure.
    #[instrument(skip(self), fields(path = %source.display()))]
    async fn import_one(&self, source: &Path) -> Result<Option<SkipReason>, LibraryError> {
        // ── 1. Extension check ────────────────────────────────────────────────
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let media_type = match self.formats.media_type_with_sniff(source, &ext) {
            Some(mt) => mt,
            None => return Ok(Some(SkipReason::UnsupportedFormat)),
        };
        let _is_video = media_type == MediaType::Video;

        // ── 2. Hash + metadata extract ───────────────────────────────────────
        // Images: extract EXIF. Videos: extract duration via GStreamer.
        let source_clone = source.to_path_buf();
        let (media_id, exif, duration_ms) =
            tokio::task::spawn_blocking(move || -> Result<_, LibraryError> {
                let id = {
                    let file = std::fs::File::open(&source_clone).map_err(LibraryError::Io)?;
                    let mut reader = std::io::BufReader::new(file);
                    let mut hasher = blake3::Hasher::new();
                    std::io::copy(&mut reader, &mut hasher).map_err(LibraryError::Io)?;
                    MediaId::new(hasher.finalize().to_hex().to_string())
                };
                let ext = source_clone
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();
                let is_vid = super::format::registry::VIDEO_EXTENSIONS.contains(&ext.as_str());
                let exif = if is_vid {
                    Default::default()
                } else {
                    extract_exif(&source_clone)
                };
                let duration_ms = if is_vid {
                    super::video_meta::extract_video_metadata(&source_clone).duration_ms
                } else {
                    None
                };
                Ok((id, exif, duration_ms))
            })
            .await
            .map_err(|e| LibraryError::Runtime(e.to_string()))??;

        // ── 3. Duplicate check via DB ─────────────────────────────────────────
        if self.db.media_exists(&media_id).await? {
            debug!(%media_id, "duplicate detected via hash");
            return Ok(Some(SkipReason::Duplicate));
        }

        // ── 4–5. Store or copy file ───────────────────────────────────────────
        let (relative_path, file_size, thumbnail_source) = match &self.mode {
            LocalStorageMode::Managed => {
                // Compute destination path inside originals/.
                let base_target =
                    compute_base_target(source, &self.originals_dir, exif.captured_at).await?;
                let target = resolve_collision(base_target);

                let rel = target
                    .strip_prefix(&self.originals_dir)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| target.to_string_lossy().into_owned());

                // Copy file into the bundle.
                if let Some(parent) = target.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(LibraryError::Io)?;
                }
                let size = tokio::fs::copy(source, &target)
                    .await
                    .map_err(LibraryError::Io)? as i64;

                (rel, size, target)
            }
            LocalStorageMode::Referenced => {
                // No copy — store the absolute path to the original.
                let abs = source.to_string_lossy().into_owned();
                let size = tokio::fs::metadata(source)
                    .await
                    .map_err(LibraryError::Io)?
                    .len() as i64;

                (abs, size, source.to_path_buf())
            }
        };

        // ── 6. Persist to database ────────────────────────────────────────────
        let original_filename = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let imported_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.db
            .insert_media(&MediaRecord {
                id: media_id.clone(),
                relative_path,
                original_filename,
                file_size,
                imported_at,
                media_type,
                taken_at: exif.captured_at,
                width: exif.width.map(|w| w as i64),
                height: exif.height.map(|h| h as i64),
                orientation: exif.orientation.unwrap_or(1),
                duration_ms,
                is_favorite: false,
                is_trashed: false,
                trashed_at: None,
            })
            .await?;

        self.db
            .insert_media_metadata(&MediaMetadataRecord {
                media_id: media_id.clone(),
                camera_make: exif.camera_make,
                camera_model: exif.camera_model,
                lens_model: exif.lens_model,
                aperture: exif.aperture,
                shutter_str: exif.shutter_str,
                iso: exif.iso,
                focal_length: exif.focal_length,
                gps_lat: exif.gps_lat,
                gps_lon: exif.gps_lon,
                gps_alt: exif.gps_alt,
                color_space: exif.color_space,
            })
            .await?;

        debug!(?thumbnail_source, "imported");

        // ── 7. Spawn thumbnail generation (non-blocking, best-effort) ─────────
        // Images use image-crate decode; videos use GStreamer poster frame.
        // Both go through the same FormatRegistry → resize → WebP pipeline.
        // Bounded by semaphore to avoid CPU/memory spikes on large imports.
        let thumb_job = ThumbnailJob::new(
            self.thumbnails_dir.clone(),
            self.db.clone(),
            self.events.clone(),
            Arc::clone(&self.formats),
        );
        let permit = Arc::clone(&self.thumbnail_semaphore);
        tokio::spawn(async move {
            let _permit = permit.acquire().await;
            thumb_job.generate(media_id, thumbnail_source).await;
        });

        // Increment summary via the Ok(None) sentinel — summary is updated
        // by the caller when it sees AssetImported was emitted.
        // We signal success by returning None after having sent the event.
        // The caller treats Ok(None) as "imported" and increments the counter.
        Ok(None)
    }
}

// ── Candidate collection ───────────────────────────────────────────────────────

/// Recursively collect all files reachable from `sources`.
pub fn collect_candidates(sources: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for source in sources {
        if source.is_file() {
            out.push(source);
        } else if source.is_dir() {
            walk_dir(&source, &mut out);
        }
    }
    out
}

pub fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(path = %dir.display(), error = %e, "could not read directory");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, out);
        } else {
            out.push(path);
        }
    }
}

// ── Path helpers ───────────────────────────────────────────────────────────────

/// Compute the base destination path (`YYYY/MM/DD/filename.ext`) for `source`
/// inside `originals_dir`.
///
/// Uses `exif_captured_at` (UTC Unix seconds) when available; falls back to
/// the file's last-modified timestamp.
async fn compute_base_target(
    source: &Path,
    originals_dir: &Path,
    exif_captured_at: Option<i64>,
) -> Result<PathBuf, LibraryError> {
    let datetime: chrono::DateTime<chrono::Local> = if let Some(ts) = exif_captured_at {
        chrono::DateTime::from_timestamp(ts, 0)
            .map(|utc| utc.with_timezone(&chrono::Local))
            .unwrap_or_else(chrono::Local::now)
    } else {
        let metadata = tokio::fs::metadata(source)
            .await
            .map_err(LibraryError::Io)?;
        let modified = metadata.modified().map_err(LibraryError::Io)?;
        modified.into()
    };

    let date_dir = originals_dir
        .join(datetime.format("%Y").to_string())
        .join(datetime.format("%m").to_string())
        .join(datetime.format("%d").to_string());

    let file_name = source.file_name().ok_or_else(|| {
        LibraryError::Bundle(format!("source has no filename: {}", source.display()))
    })?;

    Ok(date_dir.join(file_name))
}

/// Resolve filename collisions by appending `_2`, `_3`, … suffixes until the
/// path does not exist on disk. Handles same-name files with different content
/// imported in the same batch.
fn resolve_collision(base: PathBuf) -> PathBuf {
    if !base.exists() {
        return base;
    }

    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();
    let ext = base
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let dir = base.parent().unwrap_or(std::path::Path::new(""));

    let mut counter = 2u32;
    loop {
        let candidate = dir.join(format!("{stem}_{counter}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::event_bus::EventSender;
    use crate::library::format::StandardHandler;
    use tempfile::tempdir;

    fn make_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    async fn open_test_db(dir: &Path) -> Database {
        Database::open(&dir.join("db").join("test.db"))
            .await
            .unwrap()
    }

    fn test_registry() -> Arc<FormatRegistry> {
        let mut reg = FormatRegistry::new();
        reg.register(Arc::new(StandardHandler));
        Arc::new(reg)
    }

    #[tokio::test]
    async fn import_copies_jpeg_into_originals() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");
        let thumbnails = bundle_dir.path().join("thumbnails");
        let db = open_test_db(bundle_dir.path()).await;

        let photo = make_file(src_dir.path(), "photo.jpg", b"fake jpeg");

        let (tx, rx) = EventSender::test_channel();
        ImportJob::new(
            originals,
            thumbnails,
            db,
            tx,
            test_registry(),
            LocalStorageMode::Managed,
        )
        .run(vec![photo])
        .await;

        let events: Vec<_> = rx.try_iter().collect();
        let summary = events
            .iter()
            .find_map(|e| {
                if let AppEvent::ImportComplete { summary } = e {
                    Some(summary)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(summary.imported, 1);
    }

    #[tokio::test]
    async fn referenced_import_does_not_copy_and_stores_absolute_path() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");
        let thumbnails = bundle_dir.path().join("thumbnails");
        let db = open_test_db(bundle_dir.path()).await;

        let photo = make_file(src_dir.path(), "ref.jpg", b"referenced jpeg");

        let (tx, rx) = EventSender::test_channel();
        ImportJob::new(
            originals.clone(),
            thumbnails,
            db.clone(),
            tx,
            test_registry(),
            LocalStorageMode::Referenced,
        )
        .run(vec![photo.clone()])
        .await;

        let events: Vec<_> = rx.try_iter().collect();
        let summary = events
            .iter()
            .find_map(|e| {
                if let AppEvent::ImportComplete { summary: s } = e {
                    Some(s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(summary.imported, 1);

        // Referenced mode should NOT copy the file into originals/.
        assert!(!originals.exists() || std::fs::read_dir(&originals).unwrap().count() == 0);

        // The DB should store the absolute source path, not a relative one.
        use crate::library::media::{LibraryMedia, MediaFilter};
        let items = db.list_media(MediaFilter::All, None, 10).await.unwrap();
        assert_eq!(items.len(), 1);
        let stored_path = db.media_relative_path(&items[0].id).await.unwrap().unwrap();
        assert!(
            std::path::Path::new(&stored_path).is_absolute(),
            "referenced mode should store absolute path, got: {stored_path}"
        );
        assert_eq!(stored_path, photo.to_string_lossy());
    }

    #[tokio::test]
    async fn unsupported_extension_is_skipped() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");
        let thumbnails = bundle_dir.path().join("thumbnails");
        let db = open_test_db(bundle_dir.path()).await;

        let file = make_file(src_dir.path(), "document.pdf", b"not a photo");

        let (tx, rx) = EventSender::test_channel();
        ImportJob::new(
            originals,
            thumbnails,
            db,
            tx,
            test_registry(),
            LocalStorageMode::Managed,
        )
        .run(vec![file])
        .await;

        let events: Vec<_> = rx.try_iter().collect();
        let summary = events
            .iter()
            .find_map(|e| {
                if let AppEvent::ImportComplete { summary: s } = e {
                    Some(s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(summary.imported, 0);
        assert_eq!(summary.skipped_unsupported, 1);
    }

    #[tokio::test]
    async fn same_content_is_skipped_on_second_import() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");
        let db = open_test_db(bundle_dir.path()).await;

        let photo = make_file(src_dir.path(), "dup.jpg", b"fake jpeg content");

        // First import
        let thumbnails = bundle_dir.path().join("thumbnails");
        let (tx, rx) = EventSender::test_channel();
        ImportJob::new(
            originals.clone(),
            thumbnails.clone(),
            db.clone(),
            tx.clone(),
            test_registry(),
            LocalStorageMode::Managed,
        )
        .run(vec![photo.clone()])
        .await;

        // Second import — same content, even if renamed
        let photo2 = make_file(src_dir.path(), "dup_renamed.jpg", b"fake jpeg content");
        ImportJob::new(
            originals,
            thumbnails,
            db,
            tx,
            test_registry(),
            LocalStorageMode::Managed,
        )
        .run(vec![photo2])
        .await;

        let events: Vec<_> = rx.try_iter().collect();
        let summaries: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let AppEvent::ImportComplete { summary: s } = e {
                    Some(s)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(summaries[0].imported, 1);
        assert_eq!(summaries[1].skipped_duplicates, 1);
    }

    #[tokio::test]
    async fn directory_sources_are_walked_recursively() {
        let src_dir = tempdir().unwrap();
        let sub = src_dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();

        make_file(src_dir.path(), "a.jpg", b"photo a");
        make_file(&sub, "b.png", b"photo b");
        make_file(src_dir.path(), "skip.txt", b"text");

        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");
        let thumbnails = bundle_dir.path().join("thumbnails");
        let db = open_test_db(bundle_dir.path()).await;

        let (tx, rx) = EventSender::test_channel();
        ImportJob::new(
            originals,
            thumbnails,
            db,
            tx,
            test_registry(),
            LocalStorageMode::Managed,
        )
        .run(vec![src_dir.path().to_path_buf()])
        .await;

        let events: Vec<_> = rx.try_iter().collect();
        let summary = events
            .iter()
            .find_map(|e| {
                if let AppEvent::ImportComplete { summary: s } = e {
                    Some(s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(summary.imported, 2);
        assert_eq!(summary.skipped_unsupported, 1);
    }

    #[tokio::test]
    async fn video_file_is_imported_as_video_type() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");
        let thumbnails = bundle_dir.path().join("thumbnails");
        let db = open_test_db(bundle_dir.path()).await;

        make_file(src_dir.path(), "clip.mp4", b"fake video");

        let (tx, rx) = EventSender::test_channel();
        ImportJob::new(
            originals,
            thumbnails,
            db.clone(),
            tx,
            test_registry(),
            LocalStorageMode::Managed,
        )
        .run(vec![src_dir.path().to_path_buf()])
        .await;

        let events: Vec<_> = rx.try_iter().collect();
        let summary = events
            .iter()
            .find_map(|e| {
                if let AppEvent::ImportComplete { summary: s } = e {
                    Some(s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(summary.imported, 1);

        // Verify it's stored as Video type.
        use crate::library::media::{LibraryMedia, MediaFilter};
        let items = db.list_media(MediaFilter::All, None, 10).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].media_type, MediaType::Video);
    }
}

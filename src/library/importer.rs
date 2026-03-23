use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Instant;

use tracing::{debug, info, instrument, warn};

use super::error::LibraryError;
use super::event::LibraryEvent;
use super::import::{ImportSummary, SkipReason, SUPPORTED_EXTENSIONS};

/// Drives a single import run for the local backend.
///
/// Created by `LocalLibrary::import()`, which calls [`ImportJob::run`].
/// All file I/O is dispatched to a blocking thread via `gio::spawn_blocking`
/// to avoid stalling the async executor.
pub struct ImportJob {
    /// Root `originals/` directory inside the bundle.
    originals_dir: PathBuf,
    /// Shared event sender for the lifetime of the backend.
    events: Sender<LibraryEvent>,
}

impl ImportJob {
    pub fn new(originals_dir: PathBuf, events: Sender<LibraryEvent>) -> Self {
        Self {
            originals_dir,
            events,
        }
    }

    /// Execute the import: collect candidates, copy files, emit events.
    #[instrument(skip(self, sources), fields(source_count = sources.len()))]
    pub async fn run(self, sources: Vec<PathBuf>) -> Result<(), LibraryError> {
        let start = Instant::now();

        // ── 1. Collect candidate files (blocking walk) ────────────────────────
        let originals_dir = self.originals_dir.clone();
        let candidates = gio::spawn_blocking(move || collect_candidates(sources))
            .await
            .map_err(|e| LibraryError::Bundle(format!("candidate collection panicked: {e}")))?;

        let total = candidates.len();
        info!(total, "import candidates collected");

        // ── 2. Copy each file ─────────────────────────────────────────────────
        let mut summary = ImportSummary::default();

        for (idx, path) in candidates.into_iter().enumerate() {
            let current = idx + 1;

            self.events
                .send(LibraryEvent::ImportProgress { current, total })
                .ok();

            match self.import_one(&path, &originals_dir).await {
                Ok(Some(skip)) => {
                    debug!(?path, ?skip, "skipped");
                    match skip {
                        SkipReason::Duplicate => summary.skipped_duplicates += 1,
                        SkipReason::UnsupportedFormat => summary.skipped_unsupported += 1,
                    }
                }
                Ok(None) => {
                    debug!(?path, "imported");
                    summary.imported += 1;
                    self.events
                        .send(LibraryEvent::AssetImported { path })
                        .ok();
                }
                Err(e) => {
                    warn!(?path, error = %e, "failed to import file");
                    summary.failed += 1;
                    self.events.send(LibraryEvent::Error(e)).ok();
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

        self.events
            .send(LibraryEvent::ImportComplete(summary))
            .ok();

        Ok(())
    }

    /// Import a single file. Returns `Ok(None)` on success, `Ok(Some(reason))`
    /// if skipped, or `Err` on a non-fatal I/O failure.
    async fn import_one(
        &self,
        source: &Path,
        originals_dir: &Path,
    ) -> Result<Option<SkipReason>, LibraryError> {
        // Check extension
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            return Ok(Some(SkipReason::UnsupportedFormat));
        }

        // Compute target path (blocking: needs filesystem mtime)
        let source_owned = source.to_path_buf();
        let originals_owned = originals_dir.to_path_buf();

        let target = gio::spawn_blocking(move || {
            compute_target_path(&source_owned, &originals_owned)
        })
        .await
        .map_err(|e| LibraryError::Bundle(format!("target path computation panicked: {e}")))?;

        let target = target?;

        // Duplicate check
        if target.exists() {
            return Ok(Some(SkipReason::Duplicate));
        }

        // Copy (blocking)
        let source_owned = source.to_path_buf();
        gio::spawn_blocking(move || {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&source_owned, &target)?;
            Ok::<_, std::io::Error>(())
        })
        .await
        .map_err(|e| LibraryError::Bundle(format!("copy task panicked: {e}")))?
        .map_err(LibraryError::Io)?;

        Ok(None)
    }
}

/// Recursively collect all files reachable from `sources`.
///
/// Runs on a blocking thread. Ignores entries that cannot be read.
fn collect_candidates(sources: Vec<PathBuf>) -> Vec<PathBuf> {
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

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) {
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

/// Compute the destination path for `source` inside `originals_dir`.
///
/// Uses the file's last-modified timestamp for the `YYYY/MM/DD` bucket.
/// EXIF-based dating is implemented in issue #7.
///
/// If a file with the same name already exists at the target, appends `_2`,
/// `_3`, etc. to the stem before the extension.
fn compute_target_path(source: &Path, originals_dir: &Path) -> Result<PathBuf, LibraryError> {
    let metadata = std::fs::metadata(source).map_err(LibraryError::Io)?;
    let modified: std::time::SystemTime = metadata.modified().map_err(LibraryError::Io)?;

    let datetime: chrono::DateTime<chrono::Local> = modified.into();
    let date_dir = originals_dir
        .join(datetime.format("%Y").to_string())
        .join(datetime.format("%m").to_string())
        .join(datetime.format("%d").to_string());

    let file_name = source
        .file_name()
        .ok_or_else(|| LibraryError::Bundle(format!("source has no filename: {}", source.display())))?;

    let mut target = date_dir.join(file_name);

    // Resolve filename collisions
    if target.exists() {
        let stem = source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();

        let mut counter = 2u32;
        loop {
            target = date_dir.join(format!("{stem}_{counter}{ext}"));
            if !target.exists() {
                break;
            }
            counter += 1;
        }
    }

    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tempfile::tempdir;

    fn make_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn import_copies_jpeg_into_originals() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");

        let photo = make_file(src_dir.path(), "photo.jpg", b"fake jpeg");

        let (tx, rx) = mpsc::channel();
        let job = ImportJob::new(originals.clone(), tx);
        job.run(vec![photo.clone()]).await.unwrap();

        // At least one AssetImported event
        let events: Vec<_> = rx.try_iter().collect();
        let imported = events
            .iter()
            .any(|e| matches!(e, LibraryEvent::AssetImported { .. }));
        assert!(imported, "expected AssetImported event");

        // ImportComplete with imported = 1
        let complete = events.iter().find_map(|e| {
            if let LibraryEvent::ImportComplete(s) = e { Some(s.clone()) } else { None }
        });
        assert_eq!(complete.unwrap().imported, 1);
    }

    #[tokio::test]
    async fn unsupported_extension_is_skipped() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");

        let file = make_file(src_dir.path(), "document.pdf", b"not a photo");

        let (tx, rx) = mpsc::channel();
        let job = ImportJob::new(originals, tx);
        job.run(vec![file]).await.unwrap();

        let events: Vec<_> = rx.try_iter().collect();
        let complete = events.iter().find_map(|e| {
            if let LibraryEvent::ImportComplete(s) = e { Some(s.clone()) } else { None }
        });
        let summary = complete.unwrap();
        assert_eq!(summary.imported, 0);
        assert_eq!(summary.skipped_unsupported, 1);
    }

    #[tokio::test]
    async fn duplicate_filename_is_skipped() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");

        let photo = make_file(src_dir.path(), "dup.jpg", b"fake jpeg");

        let (tx, rx) = mpsc::channel();
        // First import
        let job = ImportJob::new(originals.clone(), tx.clone());
        job.run(vec![photo.clone()]).await.unwrap();

        // Second import of same file
        let job2 = ImportJob::new(originals, tx);
        job2.run(vec![photo]).await.unwrap();

        let events: Vec<_> = rx.try_iter().collect();
        let summaries: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let LibraryEvent::ImportComplete(s) = e { Some(s.clone()) } else { None }
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

        make_file(src_dir.path(), "a.jpg", b"a");
        make_file(&sub, "b.png", b"b");
        make_file(src_dir.path(), "skip.txt", b"text");

        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");

        let (tx, rx) = mpsc::channel();
        let job = ImportJob::new(originals, tx);
        job.run(vec![src_dir.path().to_path_buf()]).await.unwrap();

        let events: Vec<_> = rx.try_iter().collect();
        let complete = events.iter().find_map(|e| {
            if let LibraryEvent::ImportComplete(s) = e { Some(s.clone()) } else { None }
        });
        let summary = complete.unwrap();
        assert_eq!(summary.imported, 2);            // a.jpg + b.png
        assert_eq!(summary.skipped_unsupported, 1); // skip.txt
    }
}

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Instant;

use tracing::{debug, info, instrument, warn};

use super::error::LibraryError;
use super::event::LibraryEvent;
use super::import::{ImportSummary, SkipReason, SUPPORTED_EXTENSIONS};

/// Drives a single import run for the local backend.
///
/// [`ImportJob::run`] is **synchronous** and is intended to be called from a
/// background thread spawned by `LocalLibrary::import()`. Results are
/// communicated back through the existing [`Sender<LibraryEvent>`] so the GTK
/// layer receives progress without any extra channel wiring.
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

    /// Execute the import synchronously.
    ///
    /// Called on a background thread — never call this on the GTK main thread.
    #[instrument(skip(self, sources), fields(source_count = sources.len()))]
    pub fn run(self, sources: Vec<PathBuf>) {
        let start = Instant::now();

        // ── 1. Collect candidate files ────────────────────────────────────────
        let candidates = collect_candidates(sources);
        let total = candidates.len();
        info!(total, "import candidates collected");

        // ── 2. Copy each file ─────────────────────────────────────────────────
        let mut summary = ImportSummary::default();

        for (idx, path) in candidates.into_iter().enumerate() {
            let current = idx + 1;
            self.events
                .send(LibraryEvent::ImportProgress { current, total })
                .ok();

            match self.import_one(&path) {
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
    }

    /// Import a single file. Returns `Ok(None)` on success, `Ok(Some(reason))`
    /// if skipped, or `Err` on a non-fatal I/O failure.
    fn import_one(&self, source: &Path) -> Result<Option<SkipReason>, LibraryError> {
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            return Ok(Some(SkipReason::UnsupportedFormat));
        }

        let base_target = compute_base_target(source, &self.originals_dir)?;

        if base_target.exists() {
            return Ok(Some(SkipReason::Duplicate));
        }

        let target = resolve_collision(base_target);

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(LibraryError::Io)?;
        }
        std::fs::copy(source, &target).map_err(LibraryError::Io)?;

        Ok(None)
    }
}

/// Recursively collect all files reachable from `sources`.
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
/// Returns the base `YYYY/MM/DD/filename.ext` path without collision resolution.
/// Uses the file's last-modified timestamp for the date bucket.
/// EXIF-based dating is implemented in issue #7.
fn compute_base_target(source: &Path, originals_dir: &Path) -> Result<PathBuf, LibraryError> {
    let metadata = std::fs::metadata(source).map_err(LibraryError::Io)?;
    let modified = metadata.modified().map_err(LibraryError::Io)?;

    let datetime: chrono::DateTime<chrono::Local> = modified.into();
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
/// path does not exist on disk.
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
    use std::sync::mpsc;
    use tempfile::tempdir;

    fn make_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn import_copies_jpeg_into_originals() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");

        let photo = make_file(src_dir.path(), "photo.jpg", b"fake jpeg");

        let (tx, rx) = mpsc::channel();
        ImportJob::new(originals, tx).run(vec![photo]);

        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.iter().any(|e| matches!(e, LibraryEvent::AssetImported { .. })));

        let summary = events.iter().find_map(|e| {
            if let LibraryEvent::ImportComplete(s) = e { Some(s) } else { None }
        }).unwrap();
        assert_eq!(summary.imported, 1);
    }

    #[test]
    fn unsupported_extension_is_skipped() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");

        let file = make_file(src_dir.path(), "document.pdf", b"not a photo");

        let (tx, rx) = mpsc::channel();
        ImportJob::new(originals, tx).run(vec![file]);

        let events: Vec<_> = rx.try_iter().collect();
        let summary = events.iter().find_map(|e| {
            if let LibraryEvent::ImportComplete(s) = e { Some(s) } else { None }
        }).unwrap();
        assert_eq!(summary.imported, 0);
        assert_eq!(summary.skipped_unsupported, 1);
    }

    #[test]
    fn duplicate_filename_is_skipped_on_second_import() {
        let src_dir = tempdir().unwrap();
        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");

        let photo = make_file(src_dir.path(), "dup.jpg", b"fake jpeg");

        let (tx, rx) = mpsc::channel();
        ImportJob::new(originals.clone(), tx.clone()).run(vec![photo.clone()]);
        ImportJob::new(originals, tx).run(vec![photo]);

        let events: Vec<_> = rx.try_iter().collect();
        let summaries: Vec<_> = events.iter().filter_map(|e| {
            if let LibraryEvent::ImportComplete(s) = e { Some(s) } else { None }
        }).collect();

        assert_eq!(summaries[0].imported, 1);
        assert_eq!(summaries[1].skipped_duplicates, 1);
    }

    #[test]
    fn directory_sources_are_walked_recursively() {
        let src_dir = tempdir().unwrap();
        let sub = src_dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();

        make_file(src_dir.path(), "a.jpg", b"a");
        make_file(&sub, "b.png", b"b");
        make_file(src_dir.path(), "skip.txt", b"text");

        let bundle_dir = tempdir().unwrap();
        let originals = bundle_dir.path().join("originals");

        let (tx, rx) = mpsc::channel();
        ImportJob::new(originals, tx).run(vec![src_dir.path().to_path_buf()]);

        let events: Vec<_> = rx.try_iter().collect();
        let summary = events.iter().find_map(|e| {
            if let LibraryEvent::ImportComplete(s) = e { Some(s) } else { None }
        }).unwrap();
        assert_eq!(summary.imported, 2);
        assert_eq!(summary.skipped_unsupported, 1);
    }
}

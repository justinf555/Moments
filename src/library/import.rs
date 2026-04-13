use std::path::PathBuf;

use async_trait::async_trait;

use super::error::LibraryError;

/// Reason a source file was skipped without being imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// A file with the same name already exists at the computed target path.
    ///
    /// Replaced by content-hash detection in issue #22.
    Duplicate,

    /// The file extension is not in [`SUPPORTED_EXTENSIONS`].
    UnsupportedFormat,
}

/// Summary returned inside [`AppEvent::ImportComplete`](crate::app_event::AppEvent::ImportComplete).
#[derive(Debug, Clone, Default)]
pub struct ImportSummary {
    /// Number of files successfully copied into the library.
    pub imported: usize,
    /// Files skipped because a same-named file already exists at the target.
    pub skipped_duplicates: usize,
    /// Files skipped because their extension is not supported.
    pub skipped_unsupported: usize,
    /// Files that failed with a non-fatal I/O error.
    pub failed: usize,
    /// Wall-clock seconds the import took.
    pub elapsed_secs: f64,
}

/// Feature trait for importing media into the library.
///
/// Implemented by every backend that supports local import. The GTK layer
/// calls `library.import(sources)` and observes progress via the event bus.
#[async_trait]
pub trait LibraryImport: Send + Sync {
    /// Import files or directories into the library.
    ///
    /// `sources` may contain individual files or directories; directories are
    /// walked recursively. Progress events and the terminal
    /// `ImportComplete` are sent through the backend's event sender.
    async fn import(&self, sources: Vec<PathBuf>) -> Result<(), LibraryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_summary_default_is_zero() {
        let s = ImportSummary::default();
        assert_eq!(s.imported, 0);
        assert_eq!(s.skipped_duplicates, 0);
        assert_eq!(s.skipped_unsupported, 0);
        assert_eq!(s.failed, 0);
    }

    #[test]
    fn skip_reason_variants_are_distinct() {
        assert_ne!(SkipReason::Duplicate, SkipReason::UnsupportedFormat);
    }
}

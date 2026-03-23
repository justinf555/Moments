use std::path::PathBuf;

use async_trait::async_trait;

use super::error::LibraryError;

/// File extensions recognised as importable media.
///
/// Checked case-insensitively. Files with other extensions are skipped and
/// counted in [`ImportSummary::skipped_unsupported`].
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "heic", "heif", "tiff", "tif", "webp", "mp4", "mov",
];

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

/// Summary returned inside [`super::event::LibraryEvent::ImportComplete`].
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
/// calls `library.import(sources)` and observes progress via the shared
/// `LibraryEvent` channel that was established when the library was opened.
#[async_trait]
pub trait LibraryImport: Send + Sync {
    /// Import files or directories into the library.
    ///
    /// `sources` may contain individual files or directories; directories are
    /// walked recursively. Progress events ([`super::event::LibraryEvent::ImportProgress`],
    /// [`super::event::LibraryEvent::AssetImported`]) and the terminal
    /// [`super::event::LibraryEvent::ImportComplete`] are sent through the
    /// backend's existing event sender.
    async fn import(&self, sources: Vec<PathBuf>) -> Result<(), LibraryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_extensions_are_lowercase() {
        for ext in SUPPORTED_EXTENSIONS {
            assert_eq!(*ext, ext.to_lowercase(), "extension should be lowercase: {ext}");
        }
    }

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

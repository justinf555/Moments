use crate::library::media::MediaId;

/// Reason a source file was skipped without being imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// A file with the same content hash already exists in the library.
    Duplicate,

    /// The file extension is not recognised by the format registry.
    UnsupportedFormat,
}

/// Per-file progress update delivered via the progress callback.
#[derive(Debug, Clone)]
pub struct ImportProgress {
    /// 1-based index of the current file being processed.
    pub current: usize,
    /// Total number of candidate files discovered.
    pub total: usize,
    /// Files successfully imported so far.
    pub imported: usize,
    /// Files skipped so far (duplicates + unsupported).
    pub skipped: usize,
    /// Files that failed so far.
    pub failed: usize,
    /// The ID of the just-imported asset (set on successful import).
    pub imported_id: Option<MediaId>,
}

/// Summary of a completed import run, returned by [`super::ImportPipeline::run`].
#[derive(Debug, Clone, Default)]
pub struct ImportSummary {
    /// Number of files successfully imported into the library.
    pub imported: usize,
    /// Files skipped because their content hash already exists.
    pub skipped_duplicates: usize,
    /// Files skipped because their extension is not supported.
    pub skipped_unsupported: usize,
    /// Files that failed with a non-fatal I/O error.
    pub failed: usize,
    /// Wall-clock seconds the import took.
    pub elapsed_secs: f64,
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

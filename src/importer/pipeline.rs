use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, info, instrument, warn};

use super::builder::ImportPipelineBuilder;
use super::error::ImportError;
use super::types::{ImportProgress, ImportSummary};
use super::{discovery, filter, hasher, metadata, persistence, thumbnail, types::SkipReason};
use crate::library::config::LocalStorageMode;
use crate::library::format::FormatRegistry;
use crate::library::Library;

/// Progress callback type — invoked after each file is processed.
pub type ProgressFn = Box<dyn Fn(ImportProgress) + Send + Sync>;

/// Import pipeline for local media files.
///
/// Each file flows through every step inline: filter → hash → deduplicate →
/// extract metadata → persist file → persist records → generate thumbnail.
///
/// Ephemeral — created on demand when the user triggers an import, runs to
/// completion, then gets dropped. Use [`ImportPipeline::builder()`] to construct.
pub struct ImportPipeline {
    pub(super) originals_dir: PathBuf,
    pub(super) thumbnails_dir: PathBuf,
    /// Provides media, metadata, and thumbnail services.
    pub(super) library: Arc<Library>,
    pub(super) formats: Arc<FormatRegistry>,
    pub(super) mode: LocalStorageMode,
    pub(super) on_progress: Option<ProgressFn>,
}

impl ImportPipeline {
    /// Start building an [`ImportPipeline`].
    pub fn builder() -> ImportPipelineBuilder {
        ImportPipelineBuilder::new()
    }

    /// Execute the import pipeline.
    ///
    /// Consumes the pipeline. Returns the final [`ImportSummary`] when all
    /// files have been processed. Must be called from a Tokio async context.
    #[instrument(skip(self, sources), fields(source_count = sources.len()))]
    pub async fn run(self, sources: Vec<PathBuf>) -> ImportSummary {
        let start = Instant::now();

        // ── 1. Discover candidate files ──────────────────────────────
        let candidates = discovery::collect_candidates(sources);
        let total = candidates.len();
        info!(total, "import candidates collected");

        // ── 2. Process each file through the pipeline ────────────────
        let mut summary = ImportSummary::default();

        for (idx, path) in candidates.into_iter().enumerate() {
            let current = idx + 1;

            if let Some(ref callback) = self.on_progress {
                callback(ImportProgress {
                    current,
                    total,
                    imported: summary.imported,
                    skipped: summary.skipped_duplicates + summary.skipped_unsupported,
                    failed: summary.failed,
                });
            }

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

        summary
    }

    /// Process a single file through all pipeline steps.
    ///
    /// Returns `Ok(Some(reason))` if skipped, `Ok(None)` on success, or `Err` on failure.
    #[instrument(skip(self), fields(path = %source.display()))]
    async fn import_one(&self, source: &Path) -> Result<Option<SkipReason>, ImportError> {
        // Step 1: Filter — check format support
        let (media_type, extension) = match filter::filter(source, &self.formats) {
            filter::FilterResult::Accepted {
                media_type,
                extension,
            } => (media_type, extension),
            filter::FilterResult::Unsupported => {
                return Ok(Some(SkipReason::UnsupportedFormat));
            }
        };

        // Step 2: Hash — compute BLAKE3 content hash
        let media_id = hasher::hash_file(source).await?;

        // Step 3: Deduplicate — check if hash already exists
        if self.library.media_exists(&media_id).await? {
            debug!(%media_id, "duplicate detected via hash");
            return Ok(Some(SkipReason::Duplicate));
        }

        // Step 4: Metadata — extract EXIF / video duration
        let extracted = metadata::extract_metadata(source, media_type, &extension).await?;

        // Step 5–6: Persist — copy/link file + insert DB records
        let result = persistence::persist(persistence::PersistParams {
            source,
            media_id: &media_id,
            media_type,
            exif: extracted.exif,
            duration_ms: extracted.duration_ms,
            originals_dir: &self.originals_dir,
            mode: &self.mode,
            media: self.library.media(),
            metadata: self.library.metadata(),
        })
        .await?;

        // Step 7: Thumbnail — decode, resize, encode, write (inline)
        thumbnail::generate_thumbnail(
            &media_id,
            &result.thumbnail_source,
            &self.thumbnails_dir,
            self.library.thumbnails(),
            &self.formats,
        )
        .await;

        Ok(None)
    }
}

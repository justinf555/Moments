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
use crate::library::media::MediaId;
use crate::library::Library;

/// Progress callback type — invoked after each file is processed.
pub type ProgressFn = Box<dyn Fn(ImportProgress) + Send + Sync>;

/// Result of processing a single file through the import pipeline.
enum ImportOneResult {
    /// File was imported successfully — contains the assigned MediaId.
    Imported(MediaId),
    /// File was skipped for the given reason.
    Skipped(SkipReason),
}

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

            match self.import_one(&path).await {
                Ok(ImportOneResult::Imported(media_id)) => {
                    summary.imported += 1;
                    if let Some(ref callback) = self.on_progress {
                        callback(ImportProgress {
                            current,
                            total,
                            imported: summary.imported,
                            skipped: summary.skipped_duplicates + summary.skipped_unsupported,
                            failed: summary.failed,
                            imported_id: Some(media_id),
                        });
                    }
                }
                Ok(ImportOneResult::Skipped(skip)) => {
                    debug!(?path, ?skip, "skipped");
                    match skip {
                        SkipReason::Duplicate => summary.skipped_duplicates += 1,
                        SkipReason::UnsupportedFormat => summary.skipped_unsupported += 1,
                    }
                    if let Some(ref callback) = self.on_progress {
                        callback(ImportProgress {
                            current,
                            total,
                            imported: summary.imported,
                            skipped: summary.skipped_duplicates + summary.skipped_unsupported,
                            failed: summary.failed,
                            imported_id: None,
                        });
                    }
                }
                Err(e) => {
                    warn!(?path, error = %e, "failed to import file");
                    summary.failed += 1;
                    if let Some(ref callback) = self.on_progress {
                        callback(ImportProgress {
                            current,
                            total,
                            imported: summary.imported,
                            skipped: summary.skipped_duplicates + summary.skipped_unsupported,
                            failed: summary.failed,
                            imported_id: None,
                        });
                    }
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
    #[instrument(skip(self), fields(path = %source.display()))]
    async fn import_one(&self, source: &Path) -> Result<ImportOneResult, ImportError> {
        // Step 1: Filter — check format support
        let (media_type, extension) = match filter::filter(source, &self.formats) {
            filter::FilterResult::Accepted {
                media_type,
                extension,
            } => (media_type, extension),
            filter::FilterResult::Unsupported => {
                return Ok(ImportOneResult::Skipped(SkipReason::UnsupportedFormat));
            }
        };

        // Step 2: Hash — compute BLAKE3 content hash for dedup
        let content_hash = hasher::hash_file(source).await?;

        // Step 3: Deduplicate — check if content hash already exists
        if self
            .library
            .media()
            .exists_by_content_hash(&content_hash)
            .await?
        {
            debug!(%content_hash, "duplicate detected via content hash");
            return Ok(ImportOneResult::Skipped(SkipReason::Duplicate));
        }

        // Step 4: Generate UUID identity
        let media_id = MediaId::generate();

        // Step 5: Metadata — extract EXIF / video duration
        let extracted = metadata::extract_metadata(source, media_type, &extension).await?;

        // Step 6–7: Persist — copy/link file + insert DB records
        let result = persistence::persist(persistence::PersistParams {
            source,
            media_id: &media_id,
            content_hash: Some(&content_hash),
            media_type,
            exif: extracted.exif,
            duration_ms: extracted.duration_ms,
            originals_dir: &self.originals_dir,
            mode: &self.mode,
            media: self.library.media(),
            metadata: self.library.metadata(),
        })
        .await?;

        // Step 8: Thumbnail — decode, resize, encode, write (inline)
        thumbnail::generate_thumbnail(
            &media_id,
            &result.thumbnail_source,
            &self.thumbnails_dir,
            self.library.thumbnails(),
            &self.formats,
        )
        .await;

        Ok(ImportOneResult::Imported(media_id))
    }
}

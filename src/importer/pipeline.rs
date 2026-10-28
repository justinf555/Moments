// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use super::builder::ImportPipelineBuilder;
use super::error::ImportError;
use super::types::{ImportProgress, ImportSummary};
use super::{discovery, filter, hasher, metadata, persistence, thumbnail, types::SkipReason};
use crate::library::config::LocalStorageMode;
use crate::library::media::MediaId;
use crate::library::Library;
use crate::renderer::pipeline::RenderPipeline;

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
    pub(super) render_pipeline: Arc<RenderPipeline>,
    pub(super) mode: LocalStorageMode,
    pub(super) on_progress: Option<ProgressFn>,
    pub(super) cancel: CancellationToken,
}

impl ImportPipeline {
    /// Start building an [`ImportPipeline`].
    pub fn builder() -> ImportPipelineBuilder {
        ImportPipelineBuilder::new()
    }

    /// Execute the import pipeline over files and/or directories.
    ///
    /// Directories are walked with [`discovery::collect_candidates`]. Consumes
    /// the pipeline and returns the final [`ImportSummary`]. Must be called
    /// from a Tokio async context.
    #[instrument(skip(self, sources), fields(source_count = sources.len()))]
    pub async fn run(self, sources: Vec<PathBuf>) -> ImportSummary {
        let candidates = discovery::collect_candidates(sources);
        self.process(candidates.files, candidates.unreadable).await
    }

    /// Execute the import pipeline over an already-discovered file list.
    ///
    /// Skips discovery: every entry is treated as a file (anything that
    /// isn't one fails or is skipped by the filter as usual). Used by the
    /// import client, which scans folders itself over GIO.
    #[instrument(skip(self, files), fields(file_count = files.len()))]
    pub async fn run_files(self, files: Vec<PathBuf>) -> ImportSummary {
        self.process(files, 0).await
    }

    async fn process(self, candidates: Vec<PathBuf>, unreadable: usize) -> ImportSummary {
        let start = Instant::now();
        let total = candidates.len();
        info!(total, unreadable, "import candidates collected");

        let mut summary = ImportSummary {
            unreadable,
            ..ImportSummary::default()
        };

        for (idx, path) in candidates.into_iter().enumerate() {
            // Checked only between files: the file in flight always runs
            // to completion, so the library never holds a half-imported
            // asset (file copied but no row, or a row without a thumbnail).
            if self.cancel.is_cancelled() {
                summary.cancelled = true;
                info!(processed = idx, remaining = total - idx, "import cancelled");
                break;
            }
            let current = idx + 1;

            let imported_id = match self.import_one(&path).await {
                Ok(ImportOneResult::Imported(media_id)) => {
                    summary.imported += 1;
                    Some(media_id)
                }
                Ok(ImportOneResult::Skipped(skip)) => {
                    debug!(?path, ?skip, "skipped");
                    match skip {
                        SkipReason::Duplicate => summary.skipped_duplicates += 1,
                        SkipReason::UnsupportedFormat => summary.skipped_unsupported += 1,
                    }
                    None
                }
                Err(e) => {
                    warn!(?path, error = %e, "failed to import file");
                    summary.failed += 1;
                    None
                }
            };

            if let Some(ref callback) = self.on_progress {
                callback(ImportProgress {
                    current,
                    total,
                    imported: summary.imported,
                    skipped: summary.skipped_duplicates + summary.skipped_unsupported,
                    failed: summary.failed,
                    imported_id,
                });
            }
        }

        summary.elapsed_secs = start.elapsed().as_secs_f64();
        info!(
            imported = summary.imported,
            skipped_duplicates = summary.skipped_duplicates,
            skipped_unsupported = summary.skipped_unsupported,
            failed = summary.failed,
            unreadable = summary.unreadable,
            cancelled = summary.cancelled,
            elapsed_secs = summary.elapsed_secs,
            "import finished"
        );

        summary
    }

    /// Process a single file through all pipeline steps.
    #[instrument(skip(self), fields(path = %source.display()))]
    async fn import_one(&self, source: &Path) -> Result<ImportOneResult, ImportError> {
        // Step 1: Filter — check format support
        let (media_type, extension) = match filter::filter(source, &self.render_pipeline) {
            filter::FilterResult::Accepted {
                media_type,
                extension,
            } => (media_type, extension),
            filter::FilterResult::Unsupported => {
                return Ok(ImportOneResult::Skipped(SkipReason::UnsupportedFormat));
            }
        };

        // Step 2: Hash — compute SHA-1 content hash for dedup
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

        // Step 8: Thumbnail — render via pipeline, write to disk.
        thumbnail::generate_thumbnail(
            &media_id,
            &result.thumbnail_source,
            &self.thumbnails_dir,
            self.library.thumbnails(),
            &self.render_pipeline,
        )
        .await;

        Ok(ImportOneResult::Imported(media_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::bundle::Bundle;
    use crate::library::config::LibraryConfig;
    use crate::library::db::Database;
    use crate::library::resolver::LocalResolver;
    use std::sync::Mutex;

    /// Library plus its originals and thumbnails directories.
    async fn open_library(dir: &Path) -> (Arc<Library>, (PathBuf, PathBuf)) {
        let bundle = Bundle::create(
            &dir.join("Test.library"),
            &LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            },
        )
        .unwrap();
        let originals = bundle.originals.clone();
        let dirs = (bundle.originals.clone(), bundle.thumbnails.clone());
        let library = Library::open(
            bundle,
            LocalStorageMode::Managed,
            Database::new(),
            Arc::new(crate::sync::outbox::NoOpRecorder),
            Arc::new(LocalResolver::new(originals, LocalStorageMode::Managed)),
        )
        .await
        .unwrap();
        (Arc::new(library), dirs)
    }

    /// Write `n` distinct 2x2 PNGs (distinct content hashes) to `dir`.
    fn write_pngs(dir: &Path, n: usize) -> Vec<PathBuf> {
        (0..n)
            .map(|i| {
                let path = dir.join(format!("photo-{i}.png"));
                let img = image::RgbImage::from_pixel(2, 2, image::Rgb([i as u8, 10, 20]));
                img.save(&path).unwrap();
                path
            })
            .collect()
    }

    #[tokio::test]
    async fn cancel_stops_before_remaining_files() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        std::fs::create_dir(&source_dir).unwrap();
        let files = write_pngs(&source_dir, 5);
        let (library, (originals, thumbnails)) = open_library(dir.path()).await;

        let token = CancellationToken::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let cancel_after = 2;
        let pipeline = ImportPipeline::builder()
            .originals_dir(originals)
            .thumbnails_dir(thumbnails)
            .library(Arc::clone(&library))
            .render_pipeline(Arc::new(RenderPipeline::new()))
            .mode(LocalStorageMode::Managed)
            .cancellation_token(token.clone())
            .on_progress({
                let seen = Arc::clone(&seen);
                move |p| {
                    seen.lock().unwrap().push(p.current);
                    if p.current == cancel_after {
                        token.cancel();
                    }
                }
            })
            .build()
            .unwrap();

        let summary = pipeline.run_files(files).await;

        assert!(summary.cancelled);
        assert_eq!(*seen.lock().unwrap(), vec![1, 2]);
        assert_eq!(
            summary.imported + summary.failed + summary.skipped_duplicates,
            cancel_after,
            "{summary:?}"
        );
        assert_eq!(summary.imported, cancel_after, "{summary:?}");
        // Only the processed files reached the library.
        let stats = library.media().library_stats().await.unwrap();
        assert_eq!(stats.photo_count, cancel_after as u64);
    }

    #[tokio::test]
    async fn uncancelled_run_processes_everything() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");
        std::fs::create_dir(&source_dir).unwrap();
        std::fs::write(source_dir.join("notes.txt"), b"not media").unwrap();
        write_pngs(&source_dir, 2);
        let (library, (originals, thumbnails)) = open_library(dir.path()).await;

        let summary = ImportPipeline::builder()
            .originals_dir(originals)
            .thumbnails_dir(thumbnails)
            .library(library)
            .render_pipeline(Arc::new(RenderPipeline::new()))
            .mode(LocalStorageMode::Managed)
            .build()
            .unwrap()
            .run(vec![source_dir])
            .await;

        assert!(!summary.cancelled);
        assert_eq!(summary.imported, 2, "{summary:?}");
        assert_eq!(summary.skipped_unsupported, 1);
    }
}

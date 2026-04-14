use std::path::PathBuf;
use std::sync::Arc;

use super::error::ImportError;
use super::pipeline::ProgressFn;
use super::ImportPipeline;
use crate::library::config::LocalStorageMode;
use crate::library::format::FormatRegistry;
use crate::library::media::MediaService;
use crate::library::metadata::MetadataService;
use crate::library::thumbnail::ThumbnailService;

/// Builder for [`ImportPipeline`].
///
/// Validates that all required dependencies are provided before
/// constructing the pipeline. Use [`ImportPipeline::builder()`] to start.
pub struct ImportPipelineBuilder {
    originals_dir: Option<PathBuf>,
    thumbnails_dir: Option<PathBuf>,
    media: Option<MediaService>,
    metadata: Option<MetadataService>,
    thumbnail: Option<ThumbnailService>,
    formats: Option<Arc<FormatRegistry>>,
    mode: Option<LocalStorageMode>,
    on_progress: Option<ProgressFn>,
}

impl ImportPipelineBuilder {
    pub(super) fn new() -> Self {
        Self {
            originals_dir: None,
            thumbnails_dir: None,
            media: None,
            metadata: None,
            thumbnail: None,
            formats: None,
            mode: None,
            on_progress: None,
        }
    }

    pub fn originals_dir(mut self, dir: PathBuf) -> Self {
        self.originals_dir = Some(dir);
        self
    }

    pub fn thumbnails_dir(mut self, dir: PathBuf) -> Self {
        self.thumbnails_dir = Some(dir);
        self
    }

    pub fn media(mut self, svc: MediaService) -> Self {
        self.media = Some(svc);
        self
    }

    pub fn metadata(mut self, svc: MetadataService) -> Self {
        self.metadata = Some(svc);
        self
    }

    pub fn thumbnail(mut self, svc: ThumbnailService) -> Self {
        self.thumbnail = Some(svc);
        self
    }

    pub fn formats(mut self, reg: Arc<FormatRegistry>) -> Self {
        self.formats = Some(reg);
        self
    }

    pub fn mode(mut self, mode: LocalStorageMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Set a callback invoked after each file is processed.
    ///
    /// The closure receives an [`super::types::ImportProgress`] with the
    /// current counts. Called on the Tokio runtime thread — the UI must
    /// marshal to the GTK main thread if updating widgets.
    pub fn on_progress(
        mut self,
        callback: impl Fn(super::types::ImportProgress) + Send + 'static,
    ) -> Self {
        self.on_progress = Some(Box::new(callback));
        self
    }

    /// Build the [`ImportPipeline`].
    ///
    /// Returns an error if any required dependency is missing.
    pub fn build(self) -> Result<ImportPipeline, ImportError> {
        let missing =
            |field: &str| ImportError::Builder(format!("missing required field `{field}`"));

        Ok(ImportPipeline {
            originals_dir: self.originals_dir.ok_or_else(|| missing("originals_dir"))?,
            thumbnails_dir: self
                .thumbnails_dir
                .ok_or_else(|| missing("thumbnails_dir"))?,
            media: self.media.ok_or_else(|| missing("media"))?,
            metadata: self.metadata.ok_or_else(|| missing("metadata"))?,
            thumbnail: self.thumbnail.ok_or_else(|| missing("thumbnail"))?,
            formats: self.formats.ok_or_else(|| missing("formats"))?,
            mode: self.mode.ok_or_else(|| missing("mode"))?,
            on_progress: self.on_progress,
        })
    }
}

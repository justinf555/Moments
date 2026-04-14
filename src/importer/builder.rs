use std::path::PathBuf;
use std::sync::Arc;

use super::error::ImportError;
use super::pipeline::ProgressFn;
use super::ImportPipeline;
use crate::library::config::LocalStorageMode;
use crate::library::format::FormatRegistry;
use crate::library::Library;

/// Builder for [`ImportPipeline`].
///
/// Validates that all required dependencies are provided before
/// constructing the pipeline. Use [`ImportPipeline::builder()`] to start.
pub struct ImportPipelineBuilder {
    originals_dir: Option<PathBuf>,
    thumbnails_dir: Option<PathBuf>,
    library: Option<Arc<dyn Library>>,
    formats: Option<Arc<FormatRegistry>>,
    mode: Option<LocalStorageMode>,
    on_progress: Option<ProgressFn>,
}

impl ImportPipelineBuilder {
    pub(super) fn new() -> Self {
        Self {
            originals_dir: None,
            thumbnails_dir: None,
            library: None,
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

    /// Set the library backend providing media, metadata, and thumbnail services.
    ///
    /// Temporary: accepts `Arc<dyn Library>` because Rust cannot upcast trait
    /// objects. Will switch to individual services (MediaService, MetadataService,
    /// ThumbnailService) once the full refactor gives the caller direct access.
    pub fn library(mut self, lib: Arc<dyn Library>) -> Self {
        self.library = Some(lib);
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
        callback: impl Fn(super::types::ImportProgress) + Send + Sync + 'static,
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
            library: self.library.ok_or_else(|| missing("library"))?,
            formats: self.formats.ok_or_else(|| missing("formats"))?,
            mode: self.mode.ok_or_else(|| missing("mode"))?,
            on_progress: self.on_progress,
        })
    }
}

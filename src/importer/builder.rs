use std::path::PathBuf;
use std::sync::Arc;

use super::error::ImportError;
use super::pipeline::ProgressFn;
use super::ImportPipeline;
use crate::library::config::LocalStorageMode;
use crate::library::Library;
use crate::renderer::pipeline::RenderPipeline;

/// Builder for [`ImportPipeline`].
///
/// Validates that all required dependencies are provided before
/// constructing the pipeline. Use [`ImportPipeline::builder()`] to start.
pub struct ImportPipelineBuilder {
    originals_dir: Option<PathBuf>,
    thumbnails_dir: Option<PathBuf>,
    library: Option<Arc<Library>>,
    render_pipeline: Option<Arc<RenderPipeline>>,
    mode: Option<LocalStorageMode>,
    on_progress: Option<ProgressFn>,
}

impl ImportPipelineBuilder {
    pub(super) fn new() -> Self {
        Self {
            originals_dir: None,
            thumbnails_dir: None,
            library: None,
            render_pipeline: None,
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
    pub fn library(mut self, lib: Arc<Library>) -> Self {
        self.library = Some(lib);
        self
    }

    pub fn render_pipeline(mut self, pipeline: Arc<RenderPipeline>) -> Self {
        self.render_pipeline = Some(pipeline);
        self
    }

    pub fn mode(mut self, mode: LocalStorageMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Set a callback invoked after each file is processed.
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
            render_pipeline: self
                .render_pipeline
                .ok_or_else(|| missing("render_pipeline"))?,
            mode: self.mode.ok_or_else(|| missing("mode"))?,
            on_progress: self.on_progress,
        })
    }
}

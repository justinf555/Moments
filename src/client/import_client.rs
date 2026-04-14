use std::path::PathBuf;
use std::sync::Arc;

use tracing::{error, info};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::importer::{ImportPipeline, ImportProgress};
use crate::library::config::LocalStorageMode;
use crate::library::format::FormatRegistry;
use crate::library::Library;

/// Client-side bridge between the import pipeline and the GTK UI.
///
/// Handles:
/// 1. **Pipeline construction** — builds the pipeline via the builder pattern
/// 2. **Threading** — spawns the pipeline on Tokio, marshals progress to GTK
/// 3. **Event emission** — sends `ImportComplete` and progress on the bus
///
/// Constructed once during library open; lives for the application lifetime.
#[derive(Clone)]
pub struct ImportClient {
    library: Arc<dyn Library>,
    originals_dir: PathBuf,
    thumbnails_dir: PathBuf,
    formats: Arc<FormatRegistry>,
    mode: LocalStorageMode,
    events: EventSender,
    tokio: tokio::runtime::Handle,
}

impl ImportClient {
    pub fn new(
        library: Arc<dyn Library>,
        originals_dir: PathBuf,
        thumbnails_dir: PathBuf,
        formats: Arc<FormatRegistry>,
        mode: LocalStorageMode,
        events: EventSender,
        tokio: tokio::runtime::Handle,
    ) -> Self {
        Self {
            library,
            originals_dir,
            thumbnails_dir,
            formats,
            mode,
            events,
            tokio,
        }
    }

    /// Start an import of the given source files/directories.
    ///
    /// Constructs the pipeline, spawns it on Tokio, and returns immediately.
    /// Progress is delivered via `AppEvent::ImportProgress` on the event bus.
    /// Completion is delivered via `AppEvent::ImportComplete`.
    pub fn import(&self, sources: Vec<PathBuf>) {
        let events = self.events.clone();
        let progress_events = self.events.clone();

        let pipeline = match ImportPipeline::builder()
            .originals_dir(self.originals_dir.clone())
            .thumbnails_dir(self.thumbnails_dir.clone())
            .library(Arc::clone(&self.library))
            .formats(Arc::clone(&self.formats))
            .mode(self.mode.clone())
            .on_progress(move |p: ImportProgress| {
                progress_events.send(AppEvent::ImportProgress {
                    current: p.current,
                    total: p.total,
                    imported: p.imported,
                    skipped: p.skipped,
                    failed: p.failed,
                });
            })
            .build()
        {
            Ok(p) => p,
            Err(e) => {
                error!("failed to build import pipeline: {e}");
                events.send(AppEvent::Error(format!("Import failed: {e}")));
                return;
            }
        };

        info!(count = sources.len(), "starting import");
        self.tokio.spawn(async move {
            let summary = pipeline.run(sources).await;
            events.send(AppEvent::ImportComplete { summary });
        });
    }
}

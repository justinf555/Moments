use std::path::PathBuf;
use std::sync::mpsc::Sender;

use async_trait::async_trait;
use tracing::{debug, info, instrument};

use super::bundle::Bundle;
use super::error::LibraryError;
use super::event::LibraryEvent;
use super::import::LibraryImport;
use super::importer::ImportJob;
use super::storage::LibraryStorage;

/// Local filesystem backend.
///
/// Originals are imported into the bundle's `originals/` subdirectory.
pub struct LocalLibrary {
    bundle: Bundle,
    events: Sender<LibraryEvent>,
}

#[async_trait]
impl LibraryStorage for LocalLibrary {
    #[instrument(skip(events), fields(path = %bundle.path.display()))]
    async fn open(bundle: Bundle, events: Sender<LibraryEvent>) -> Result<Self, LibraryError>
    where
        Self: Sized,
    {
        info!("opening local library");
        let library = Self { bundle, events };
        library
            .events
            .send(LibraryEvent::Ready)
            .map_err(|_| LibraryError::Bundle("event channel closed".to_string()))?;
        debug!("local library ready");
        Ok(library)
    }

    #[instrument(skip(self), fields(path = %self.bundle.path.display()))]
    async fn close(&self) -> Result<(), LibraryError> {
        info!("closing local library");
        self.events
            .send(LibraryEvent::ShutdownComplete)
            .map_err(|_| LibraryError::Bundle("event channel closed".to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl LibraryImport for LocalLibrary {
    #[instrument(skip(self), fields(source_count = sources.len()))]
    async fn import(&self, sources: Vec<PathBuf>) -> Result<(), LibraryError> {
        info!("starting import");
        let job = ImportJob::new(self.bundle.originals.clone(), self.events.clone());
        job.run(sources).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::config::LibraryConfig;
    use crate::library::event::LibraryEvent;
    use std::sync::mpsc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn open_sends_ready_event() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();

        let (tx, rx) = mpsc::channel();
        let _library = LocalLibrary::open(bundle, tx).await.unwrap();

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, LibraryEvent::Ready));
    }

    #[tokio::test]
    async fn close_sends_shutdown_complete() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();

        let (tx, rx) = mpsc::channel();
        let library = LocalLibrary::open(bundle, tx).await.unwrap();
        rx.try_recv().unwrap(); // consume Ready

        library.close().await.unwrap();
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, LibraryEvent::ShutdownComplete));
    }

    #[tokio::test]
    async fn import_emits_complete_event() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();

        let src_dir = tempdir().unwrap();
        std::fs::write(src_dir.path().join("img.jpg"), b"fake").unwrap();

        let (tx, rx) = mpsc::channel();
        let library = LocalLibrary::open(bundle, tx).await.unwrap();
        rx.try_recv().unwrap(); // consume Ready

        library
            .import(vec![src_dir.path().to_path_buf()])
            .await
            .unwrap();

        let events: Vec<_> = rx.try_iter().collect();
        let has_complete = events
            .iter()
            .any(|e| matches!(e, LibraryEvent::ImportComplete(_)));
        assert!(has_complete);
    }
}

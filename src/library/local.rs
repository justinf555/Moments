use std::sync::mpsc::Sender;

use async_trait::async_trait;
use tracing::{debug, info, instrument};

use super::bundle::Bundle;
use super::error::LibraryError;
use super::event::LibraryEvent;
use super::storage::LibraryStorage;
use super::Library;

/// Local filesystem backend.
///
/// Originals are imported into the bundle's `originals/` subdirectory.
/// This is a stub implementation — photo import is implemented in issue #5.
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

impl Library for LocalLibrary {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::config::LibraryConfig;
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

        // Consume the Ready event
        rx.try_recv().unwrap();

        library.close().await.unwrap();
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, LibraryEvent::ShutdownComplete));
    }
}

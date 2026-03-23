use std::path::PathBuf;
use std::sync::mpsc::Sender;

use async_trait::async_trait;
use tokio::runtime::Handle;
use tracing::{debug, info, instrument};

use super::bundle::Bundle;
use super::db::Database;
use super::error::LibraryError;
use super::event::LibraryEvent;
use super::import::LibraryImport;
use super::importer::ImportJob;
use super::media::{LibraryMedia, MediaId, MediaRecord};
use super::storage::LibraryStorage;

/// Local filesystem backend.
///
/// Holds a Tokio [`Handle`] (the shared application-level library executor)
/// and a [`Database`] connection pool. All I/O-bound work is dispatched
/// through the Tokio handle so it never blocks the GTK main thread.
pub struct LocalLibrary {
    bundle: Bundle,
    events: Sender<LibraryEvent>,
    db: Database,
    tokio: Handle,
}

#[async_trait]
impl LibraryStorage for LocalLibrary {
    #[instrument(skip(events, tokio), fields(path = %bundle.path.display()))]
    async fn open(
        bundle: Bundle,
        events: Sender<LibraryEvent>,
        tokio: Handle,
    ) -> Result<Self, LibraryError>
    where
        Self: Sized,
    {
        info!("opening local library");

        // Initialise the database on the Tokio executor. DB init is fast
        // (~1ms — schema migration only) so we block briefly here at startup.
        let db_path = bundle.database.join("moments.db");
        let db = tokio
            .spawn(async move { Database::open(&db_path).await })
            .await
            .map_err(|e| LibraryError::Runtime(e.to_string()))??;

        let library = Self {
            bundle,
            events,
            db,
            tokio,
        };
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
        let job = ImportJob::new(
            self.bundle.originals.clone(),
            self.db.clone(),
            self.events.clone(),
        );
        self.tokio.spawn(async move { job.run(sources).await });
        Ok(())
    }
}

#[async_trait]
impl LibraryMedia for LocalLibrary {
    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.db.media_exists(id).await
    }

    async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.db.insert_media(record).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::config::LibraryConfig;
    use crate::library::event::LibraryEvent;
    use std::sync::mpsc;
    use tempfile::tempdir;

    async fn open_test_library(bundle: Bundle, tx: Sender<LibraryEvent>) -> LocalLibrary {
        let handle = tokio::runtime::Handle::current();
        LocalLibrary::open(bundle, tx, handle).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_sends_ready_event() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();

        let (tx, rx) = mpsc::channel();
        let _library = open_test_library(bundle, tx).await;

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, LibraryEvent::Ready));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_sends_shutdown_complete() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();

        let (tx, rx) = mpsc::channel();
        let library = open_test_library(bundle, tx).await;
        rx.try_recv().unwrap(); // consume Ready

        library.close().await.unwrap();
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, LibraryEvent::ShutdownComplete));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn import_emits_complete_event() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();

        let src_dir = tempdir().unwrap();
        std::fs::write(src_dir.path().join("img.jpg"), b"fake").unwrap();

        let (tx, rx) = mpsc::channel();
        let library = open_test_library(bundle, tx).await;
        rx.try_recv().unwrap(); // consume Ready

        library
            .import(vec![src_dir.path().to_path_buf()])
            .await
            .unwrap();

        // import() spawns on Tokio; drain events until ImportComplete arrives.
        let has_complete = loop {
            match rx.recv().unwrap() {
                LibraryEvent::ImportComplete(_) => break true,
                _ => continue,
            }
        };
        assert!(has_complete);
    }
}

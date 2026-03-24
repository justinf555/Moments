use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::runtime::Handle;
use tracing::{debug, info, instrument};

use crate::library::bundle::Bundle;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::event::LibraryEvent;
use crate::library::format::{FormatRegistry, RawHandler, StandardHandler, VideoHandler};
use crate::library::import::LibraryImport;
use crate::library::importer::ImportJob;
use crate::library::album::{Album, AlbumId, LibraryAlbums};
use crate::library::media::{
    LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaMetadataRecord, MediaRecord,
};
use crate::library::storage::LibraryStorage;
use crate::library::thumbnail::{sharded_thumbnail_path, LibraryThumbnail};
use crate::library::viewer::LibraryViewer;

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
    formats: Arc<FormatRegistry>,
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

        let mut registry = FormatRegistry::new();
        registry.register(Arc::new(StandardHandler));
        registry.register(Arc::new(RawHandler));
        registry.register(Arc::new(VideoHandler));
        let formats = Arc::new(registry);

        let library = Self {
            bundle,
            events,
            db,
            tokio,
            formats,
        };
        // Fire-and-forget: purge items trashed > 30 days on background thread.
        {
            let db = library.db.clone();
            let originals = library.bundle.originals.clone();
            let thumbnails = library.bundle.thumbnails.clone();
            library.tokio.spawn(async move {
                const THIRTY_DAYS: i64 = 30 * 24 * 60 * 60;
                match db.expired_trash(THIRTY_DAYS).await {
                    Ok(ids) if !ids.is_empty() => {
                        info!(count = ids.len(), "auto-purging expired trash");
                        for id in &ids {
                            // Remove original file.
                            if let Ok(Some(rel)) = db.media_relative_path(id).await {
                                let path = originals.join(&rel);
                                let _ = tokio::fs::remove_file(&path).await;
                            }
                            // Remove thumbnail file.
                            let thumb = crate::library::thumbnail::sharded_thumbnail_path(&thumbnails, id);
                            let _ = tokio::fs::remove_file(&thumb).await;
                        }
                        if let Err(e) = db.delete_permanently(&ids).await {
                            tracing::error!("auto-purge DB cleanup failed: {e}");
                        }
                    }
                    Ok(_) => debug!("no expired trash to purge"),
                    Err(e) => tracing::error!("auto-purge query failed: {e}"),
                }
            });
        }

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
            self.bundle.thumbnails.clone(),
            self.db.clone(),
            self.events.clone(),
            Arc::clone(&self.formats),
        );
        self.tokio.spawn(async move { job.run(sources).await });
        Ok(())
    }
}

#[async_trait]
impl LibraryMedia for LocalLibrary {
    async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        self.db.get_media_item(id).await
    }

    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.db.media_exists(id).await
    }

    async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.db.insert_media(record).await
    }

    async fn insert_media_metadata(
        &self,
        record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        self.db.insert_media_metadata(record).await
    }

    async fn list_media(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        self.db.list_media(filter, cursor, limit).await
    }

    async fn media_metadata(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaMetadataRecord>, LibraryError> {
        self.db.media_metadata(id).await
    }

    async fn set_favorite(
        &self,
        ids: &[MediaId],
        favorite: bool,
    ) -> Result<(), LibraryError> {
        self.db.set_favorite(ids, favorite).await
    }

    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.db.trash(ids).await
    }

    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.db.restore(ids).await
    }

    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        // Remove files from disk before deleting DB rows.
        for id in ids {
            // Remove original file.
            if let Ok(Some(rel)) = self.db.media_relative_path(id).await {
                let path = self.bundle.originals.join(rel);
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    tracing::warn!(id = %id, path = %path.display(), "failed to remove original: {e}");
                }
            }
            // Remove thumbnail file.
            let thumb = self.thumbnail_path(id);
            if let Err(e) = tokio::fs::remove_file(&thumb).await {
                tracing::debug!(id = %id, "thumbnail not on disk or already removed: {e}");
            }
        }
        self.db.delete_permanently(ids).await
    }

    async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        self.db.expired_trash(max_age_secs).await
    }
}

#[async_trait]
impl LibraryViewer for LocalLibrary {
    async fn original_path(&self, id: &MediaId) -> Result<Option<std::path::PathBuf>, LibraryError> {
        let relative = self.db.media_relative_path(id).await?;
        Ok(relative.map(|rel| self.bundle.originals.join(rel)))
    }
}

#[async_trait]
impl LibraryThumbnail for LocalLibrary {
    fn thumbnail_path(&self, id: &MediaId) -> std::path::PathBuf {
        sharded_thumbnail_path(&self.bundle.thumbnails, id)
    }

    async fn insert_thumbnail_pending(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.db.insert_thumbnail_pending(id).await
    }

    async fn set_thumbnail_ready(
        &self,
        id: &MediaId,
        file_path: &str,
        generated_at: i64,
    ) -> Result<(), LibraryError> {
        self.db.set_thumbnail_ready(id, file_path, generated_at).await
    }

    async fn set_thumbnail_failed(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.db.set_thumbnail_failed(id).await
    }

    async fn thumbnail_status(
        &self,
        id: &MediaId,
    ) -> Result<Option<crate::library::thumbnail::ThumbnailStatus>, LibraryError> {
        self.db.thumbnail_status(id).await
    }
}

#[async_trait]
impl LibraryAlbums for LocalLibrary {
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        self.db.list_albums().await
    }

    async fn create_album(&self, name: &str) -> Result<AlbumId, LibraryError> {
        self.db.create_album(name).await
    }

    async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        self.db.rename_album(id, name).await
    }

    async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError> {
        self.db.delete_album(id).await
    }

    async fn add_to_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.db.add_to_album(album_id, media_ids).await
    }

    async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.db.remove_from_album(album_id, media_ids).await
    }

    async fn list_album_media(
        &self,
        album_id: &AlbumId,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        self.db.list_album_media(album_id, cursor, limit).await
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

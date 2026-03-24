use std::path::PathBuf;
use std::sync::mpsc::Sender;

use async_trait::async_trait;
use tokio::runtime::Handle;
use tracing::{debug, info, instrument};

use crate::library::album::{Album, AlbumId, LibraryAlbums};
use crate::library::bundle::Bundle;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::event::LibraryEvent;
use crate::library::immich_client::ImmichClient;
use crate::library::import::LibraryImport;
use crate::library::sync::SyncHandle;
use crate::library::media::{
    LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaMetadataRecord, MediaRecord,
};
use crate::library::storage::LibraryStorage;
use crate::library::thumbnail::{sharded_thumbnail_path, LibraryThumbnail, ThumbnailStatus};
use crate::library::viewer::LibraryViewer;

/// Immich server backend.
///
/// Follows the offline-first architecture described in
/// `docs/design-immich-backend.md`: all reads come from the local SQLite
/// cache (same `Database` struct as `LocalLibrary`), writes go to the
/// Immich API first then update the local cache, and a background
/// `SyncManager` (PR #109) keeps the cache in sync with the server.
pub struct ImmichLibrary {
    bundle: Bundle,
    client: ImmichClient,
    db: Database,
    events: Sender<LibraryEvent>,
    tokio: Handle,
    #[allow(dead_code)] // shutdown signalled in close()
    sync_handle: SyncHandle,
}

impl ImmichLibrary {
    /// Open an Immich library backed by a local cache.
    ///
    /// Called from `LibraryFactory` which builds the `ImmichClient` from
    /// the config. This constructor does not use `LibraryStorage::open`
    /// because it needs the pre-built client (the trait signature doesn't
    /// accept config).
    #[instrument(skip(client, events, tokio), fields(path = %bundle.path.display()))]
    pub async fn open(
        bundle: Bundle,
        client: ImmichClient,
        events: Sender<LibraryEvent>,
        tokio: Handle,
    ) -> Result<Self, LibraryError> {
        info!("opening immich library");

        // Open the local cache database (same schema as local backend).
        let db_path = bundle.database.join("moments.db");
        let db = tokio
            .spawn(async move { Database::open(&db_path).await })
            .await
            .map_err(|e| LibraryError::Runtime(e.to_string()))??;

        // Start the background sync engine.
        let sync_handle = SyncHandle::start(
            client.clone(),
            db.clone(),
            events.clone(),
            tokio.clone(),
        );

        let library = Self {
            bundle,
            client,
            db,
            events,
            tokio,
            sync_handle,
        };

        library
            .events
            .send(LibraryEvent::Ready)
            .map_err(|_| LibraryError::Bundle("event channel closed".to_string()))?;

        debug!("immich library ready");
        Ok(library)
    }
}

// LibraryStorage is not used directly — ImmichLibrary::open is called
// from the factory instead. We implement it to satisfy the Library
// supertrait bound.
#[async_trait]
impl LibraryStorage for ImmichLibrary {
    async fn open(
        _bundle: Bundle,
        _events: Sender<LibraryEvent>,
        _tokio: Handle,
    ) -> Result<Self, LibraryError>
    where
        Self: Sized,
    {
        // Not reachable — factory calls ImmichLibrary::open directly.
        Err(LibraryError::Immich(
            "use ImmichLibrary::open() instead of LibraryStorage::open()".to_string(),
        ))
    }

    #[instrument(skip(self))]
    async fn close(&self) -> Result<(), LibraryError> {
        info!("closing immich library");
        self.events
            .send(LibraryEvent::ShutdownComplete)
            .map_err(|_| LibraryError::Bundle("event channel closed".to_string()))?;
        Ok(())
    }
}

// ── Reads delegate to local cache DB ────────────────────────────────────────
// Writes are stubs until later PRs implement API call + cache update.

#[async_trait]
impl LibraryMedia for ImmichLibrary {
    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.db.media_exists(id).await
    }

    async fn insert_media(&self, _record: &MediaRecord) -> Result<(), LibraryError> {
        // Managed by SyncManager — not called directly for Immich.
        Ok(())
    }

    async fn insert_media_metadata(
        &self,
        _record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        // Managed by SyncManager.
        Ok(())
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
        // TODO (#103): call Immich API then update local cache.
        self.db.set_favorite(ids, favorite).await
    }

    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        // TODO (#103): call Immich API then update local cache.
        self.db.trash(ids).await
    }

    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        // TODO (#103): call Immich API then update local cache.
        self.db.restore(ids).await
    }

    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        // TODO (#103): call Immich API then update local cache.
        self.db.delete_permanently(ids).await
    }

    async fn expired_trash(&self, _max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        // Server manages trash retention — nothing to do locally.
        Ok(vec![])
    }
}

#[async_trait]
impl LibraryImport for ImmichLibrary {
    #[instrument(skip(self))]
    async fn import(&self, _sources: Vec<PathBuf>) -> Result<(), LibraryError> {
        // TODO (#106): upload to Immich server.
        Err(LibraryError::Immich(
            "import not yet implemented for Immich backend".to_string(),
        ))
    }
}

#[async_trait]
impl LibraryThumbnail for ImmichLibrary {
    fn thumbnail_path(&self, id: &MediaId) -> PathBuf {
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
    ) -> Result<Option<ThumbnailStatus>, LibraryError> {
        self.db.thumbnail_status(id).await
    }
}

#[async_trait]
impl LibraryViewer for ImmichLibrary {
    async fn original_path(
        &self,
        _id: &MediaId,
    ) -> Result<Option<PathBuf>, LibraryError> {
        // TODO (#107): download original on demand with local cache.
        Ok(None)
    }
}

#[async_trait]
impl LibraryAlbums for ImmichLibrary {
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        self.db.list_albums().await
    }

    async fn create_album(&self, _name: &str) -> Result<AlbumId, LibraryError> {
        // TODO (#105): call Immich API then update local cache.
        Err(LibraryError::Immich(
            "album creation not yet implemented for Immich backend".to_string(),
        ))
    }

    async fn rename_album(&self, _id: &AlbumId, _name: &str) -> Result<(), LibraryError> {
        Err(LibraryError::Immich(
            "album rename not yet implemented for Immich backend".to_string(),
        ))
    }

    async fn delete_album(&self, _id: &AlbumId) -> Result<(), LibraryError> {
        Err(LibraryError::Immich(
            "album delete not yet implemented for Immich backend".to_string(),
        ))
    }

    async fn add_to_album(
        &self,
        _album_id: &AlbumId,
        _media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        Err(LibraryError::Immich(
            "add to album not yet implemented for Immich backend".to_string(),
        ))
    }

    async fn remove_from_album(
        &self,
        _album_id: &AlbumId,
        _media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        Err(LibraryError::Immich(
            "remove from album not yet implemented for Immich backend".to_string(),
        ))
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

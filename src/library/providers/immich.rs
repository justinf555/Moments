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
use crate::library::faces::{LibraryFaces, Person, PersonId};
use crate::library::immich_client::ImmichClient;
use crate::library::import::LibraryImport;
use crate::library::sync::SyncHandle;
use crate::library::media::{
    LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaMetadataRecord, MediaRecord,
    MediaType,
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

        // Fire-and-forget: evict old originals cache entries if over the limit.
        // Read GSettings on the GTK thread before spawning onto Tokio.
        {
            use gtk::prelude::SettingsExt;
            let settings = gtk::gio::Settings::new("io.github.justinf555.Moments");
            let max_mb = settings.uint("originals-cache-max-mb");
            let originals_dir = bundle.originals.clone();
            tokio.spawn(async move {
                evict_originals_cache(&originals_dir, max_mb).await;
            });
        }

        // Start the background sync engine.
        let sync_handle = SyncHandle::start(
            client.clone(),
            db.clone(),
            events.clone(),
            bundle.thumbnails.clone(),
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
        self.sync_handle.shutdown();
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
    async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        self.db.get_media_item(id).await
    }

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
        // Write-through: API first, then local cache.
        let api_ids: Vec<String> = ids.iter().map(|id| id.as_str().to_owned()).collect();
        self.client
            .put_no_content(
                "/assets",
                &serde_json::json!({
                    "ids": api_ids,
                    "isFavorite": favorite,
                }),
            )
            .await?;
        self.db.set_favorite(ids, favorite).await
    }

    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        let api_ids: Vec<String> = ids.iter().map(|id| id.as_str().to_owned()).collect();
        self.client
            .delete_with_body(
                "/assets",
                &serde_json::json!({ "ids": api_ids }),
            )
            .await?;
        self.db.trash(ids).await
    }

    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        let api_ids: Vec<String> = ids.iter().map(|id| id.as_str().to_owned()).collect();
        self.client
            .post_no_content(
                "/trash/restore/assets",
                &serde_json::json!({ "ids": api_ids }),
            )
            .await?;
        self.db.restore(ids).await
    }

    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        let api_ids: Vec<String> = ids.iter().map(|id| id.as_str().to_owned()).collect();
        self.client
            .delete_with_body(
                "/assets",
                &serde_json::json!({ "ids": api_ids, "force": true }),
            )
            .await?;
        self.db.delete_permanently(ids).await
    }

    async fn expired_trash(&self, _max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        // Server manages trash retention — nothing to do locally.
        Ok(vec![])
    }
}

#[async_trait]
impl LibraryImport for ImmichLibrary {
    #[instrument(skip(self), fields(source_count = sources.len()))]
    async fn import(&self, sources: Vec<PathBuf>) -> Result<(), LibraryError> {
        info!("starting Immich upload");
        let job = crate::library::immich_importer::ImmichImportJob {
            client: self.client.clone(),
            db: self.db.clone(),
            events: self.events.clone(),
        };
        self.tokio.spawn(async move { job.run(sources).await });
        Ok(())
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
    #[instrument(skip(self))]
    async fn original_path(
        &self,
        id: &MediaId,
    ) -> Result<Option<PathBuf>, LibraryError> {
        // Get the original filename for its extension (needed by image decoders).
        let filename = self.db.media_original_filename(id).await?;
        let ext = filename
            .as_deref()
            .and_then(|f| std::path::Path::new(f).extension())
            .and_then(|e| e.to_str())
            .unwrap_or("dat");

        let cache_path = sharded_original_path(&self.bundle.originals, id, ext);

        // Return cached file if it exists.
        if cache_path.exists() {
            debug!(id = %id, "original cache hit");
            return Ok(Some(cache_path));
        }

        // Download from Immich.
        let api_path = format!("/assets/{}/original", id.as_str());
        info!(id = %id, "downloading original from Immich");
        let bytes = self.client.get_bytes(&api_path).await?;

        // Write to cache.
        if let Some(parent) = cache_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(LibraryError::Io)?;
        }
        let size = bytes.len();
        tokio::fs::write(&cache_path, &bytes)
            .await
            .map_err(LibraryError::Io)?;
        debug!(id = %id, size_bytes = size, ext, "original cached");

        Ok(Some(cache_path))
    }
}

/// Compute the sharded cache path for an original file.
///
/// Includes the file extension so image/video decoders can identify the format.
/// Path: `originals/{hex[..2]}/{hex[2..4]}/{id}.{ext}`
fn sharded_original_path(originals_dir: &std::path::Path, id: &MediaId, ext: &str) -> PathBuf {
    let hex = id.as_str();
    originals_dir
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(format!("{hex}.{ext}"))
}

/// Evict oldest cached originals until the cache is under the configured limit.
///
/// Reads the limit from GSettings (`originals-cache-max-mb`). Walks the
/// originals directory, sorts by modification time, and deletes oldest
/// files first. Runs on library open as a background task.
async fn evict_originals_cache(originals_dir: &std::path::Path, max_mb: u32) {
    if max_mb == 0 {
        debug!("originals cache eviction disabled (max_mb=0)");
        return;
    }
    let max_bytes = max_mb as u64 * 1024 * 1024;

    // Walk the cache directory and collect file info.
    let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total_size: u64 = 0;

    if let Ok(mut read_dir) = tokio::fs::read_dir(originals_dir).await {
        while let Ok(Some(shard1)) = read_dir.next_entry().await {
            if !shard1.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if let Ok(mut shard1_dir) = tokio::fs::read_dir(shard1.path()).await {
                while let Ok(Some(shard2)) = shard1_dir.next_entry().await {
                    if !shard2.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    if let Ok(mut shard2_dir) = tokio::fs::read_dir(shard2.path()).await {
                        while let Ok(Some(file)) = shard2_dir.next_entry().await {
                            if let Ok(meta) = file.metadata().await {
                                if meta.is_file() {
                                    let size = meta.len();
                                    let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                                    total_size += size;
                                    entries.push((file.path(), size, modified));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if total_size <= max_bytes {
        debug!(
            total_mb = total_size / (1024 * 1024),
            max_mb,
            files = entries.len(),
            "originals cache within limit"
        );
        return;
    }

    // Sort oldest first.
    entries.sort_by_key(|(_, _, modified)| *modified);

    let mut evicted_count = 0u64;
    let mut evicted_bytes = 0u64;

    for (path, size, _) in &entries {
        if total_size <= max_bytes {
            break;
        }
        if let Err(e) = tokio::fs::remove_file(&path).await {
            tracing::warn!(path = %path.display(), "failed to evict cached original: {e}");
            continue;
        }
        total_size -= size;
        evicted_bytes += size;
        evicted_count += 1;
    }

    info!(
        evicted_files = evicted_count,
        evicted_mb = evicted_bytes / (1024 * 1024),
        remaining_mb = total_size / (1024 * 1024),
        "originals cache eviction complete"
    );
}

#[async_trait]
impl LibraryAlbums for ImmichLibrary {
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        self.db.list_albums().await
    }

    async fn create_album(&self, name: &str) -> Result<AlbumId, LibraryError> {
        // API first: POST /albums → returns album with server-generated ID.
        let resp: serde_json::Value = self
            .client
            .post("/albums", &serde_json::json!({ "albumName": name }))
            .await?;
        let server_id = resp["id"]
            .as_str()
            .ok_or_else(|| LibraryError::Immich("no id in create album response".to_string()))?
            .to_owned();

        // Cache locally with the server-generated ID.
        let now = chrono::Utc::now().timestamp();
        self.db.upsert_album(&server_id, name, now, now).await?;

        Ok(AlbumId::from_raw(server_id))
    }

    async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        let path = format!("/albums/{}", id.as_str());
        self.client
            .patch_no_content(&path, &serde_json::json!({ "albumName": name }))
            .await?;
        self.db.rename_album(id, name).await
    }

    async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError> {
        let path = format!("/albums/{}", id.as_str());
        self.client.delete_no_content(&path).await?;
        self.db.delete_album(id).await
    }

    async fn add_to_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        let ids: Vec<String> = media_ids.iter().map(|id| id.as_str().to_owned()).collect();
        let path = format!("/albums/{}/assets", album_id.as_str());
        self.client
            .put_no_content(&path, &serde_json::json!({ "ids": ids }))
            .await?;
        self.db.add_to_album(album_id, media_ids).await
    }

    async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        let ids: Vec<String> = media_ids.iter().map(|id| id.as_str().to_owned()).collect();
        let path = format!("/albums/{}/assets", album_id.as_str());
        self.client
            .delete_with_body(&path, &serde_json::json!({ "ids": ids }))
            .await?;
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

#[async_trait]
impl LibraryFaces for ImmichLibrary {
    async fn list_people(
        &self,
        _include_hidden: bool,
        _include_unnamed: bool,
    ) -> Result<Vec<Person>, LibraryError> {
        // TODO: wire to self.db once db/faces.rs is implemented (#181)
        Ok(Vec::new())
    }

    async fn list_media_for_person(
        &self,
        _person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError> {
        Ok(Vec::new())
    }

    async fn rename_person(
        &self,
        _person_id: &PersonId,
        _name: &str,
    ) -> Result<(), LibraryError> {
        Ok(())
    }

    async fn set_person_hidden(
        &self,
        _person_id: &PersonId,
        _hidden: bool,
    ) -> Result<(), LibraryError> {
        Ok(())
    }

    async fn merge_people(
        &self,
        _target: &PersonId,
        _sources: &[PersonId],
    ) -> Result<(), LibraryError> {
        Ok(())
    }

    fn person_thumbnail_path(&self, person_id: &PersonId) -> Option<std::path::PathBuf> {
        let path = self.bundle.thumbnails.join("people").join(format!("{}.jpg", person_id.as_str()));
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }
}

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use async_trait::async_trait;
use tokio::runtime::Handle;
use tracing::{debug, info, instrument};

use crate::library::album::{Album, AlbumId, LibraryAlbums};
use crate::library::bundle::Bundle;
use crate::library::db::Database;
use crate::library::editing::{EditState, LibraryEditing};
use crate::library::error::LibraryError;
use crate::library::event::LibraryEvent;
use crate::library::faces::{LibraryFaces, Person, PersonId};
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
    sync_handle: SyncHandle,
    cache_limit_tx: tokio::sync::watch::Sender<u32>,
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

        // Periodic cache evictor — delayed startup, runs every 10 minutes.
        let cache_limit_tx = {
            use gtk::prelude::SettingsExt;
            let settings = gtk::gio::Settings::new("io.github.justinf555.Moments");
            let max_mb = settings.uint("originals-cache-max-mb");
            let (tx, rx) = tokio::sync::watch::channel(max_mb);
            let originals_dir = bundle.originals.clone();
            tokio.spawn(async move {
                run_cache_evictor(originals_dir, rx).await;
            });
            tx
        };

        // Read sync interval from GSettings (must be on GTK thread).
        let sync_interval_secs = {
            use gtk::prelude::SettingsExt;
            let settings = gtk::gio::Settings::new("io.github.justinf555.Moments");
            settings.uint("sync-interval-seconds") as u64
        };

        // Start the background sync engine.
        let sync_handle = SyncHandle::start(
            client.clone(),
            db.clone(),
            events.clone(),
            bundle.thumbnails.clone(),
            tokio.clone(),
            sync_interval_secs,
        );

        let library = Self {
            bundle,
            client,
            db,
            events,
            tokio,
            sync_handle,
            cache_limit_tx,
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

    fn set_sync_interval(&self, secs: u64) {
        self.sync_handle.set_interval(secs);
    }

    fn set_cache_limit(&self, mb: u32) {
        let _ = self.cache_limit_tx.send(mb);
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

    async fn library_stats(&self) -> Result<crate::library::db::LibraryStats, LibraryError> {
        let mut stats = self.db.library_stats().await?;

        // Calculate originals cache disk usage.
        let originals_dir = self.bundle.originals.clone();
        if let Ok(size) = tokio::task::spawn_blocking(move || dir_size(&originals_dir)).await {
            stats.cache_used_bytes = size;
        }

        // Fetch server-side statistics.
        let mut server = crate::library::db::ServerStats {
            server_photos: 0,
            server_videos: 0,
            disk_size: 0,
            disk_use: 0,
            disk_usage_percentage: 0.0,
        };

        // GET /assets/statistics — user-scoped photo/video counts.
        if let Ok(asset_stats) = self.client.get::<AssetStatistics>("/assets/statistics").await {
            server.server_photos = asset_stats.images as u64;
            server.server_videos = asset_stats.videos as u64;
        }

        // GET /server/statistics — Immich-specific storage usage (admin only).
        // Falls back to /server/storage (OS-level) if the user is not an admin.
        if let Ok(stats_resp) = self.client.get::<ServerStatistics>("/server/statistics").await {
            server.disk_use = stats_resp.usage as u64;
            // /server/statistics doesn't include total disk size — fetch from /server/storage.
            if let Ok(storage) = self.client.get::<ServerStorage>("/server/storage").await {
                server.disk_size = storage.disk_size_raw;
                server.disk_usage_percentage = if storage.disk_size_raw > 0 {
                    (stats_resp.usage as f64 / storage.disk_size_raw as f64) * 100.0
                } else {
                    0.0
                };
            }
        } else if let Ok(storage) = self.client.get::<ServerStorage>("/server/storage").await {
            // Fallback for non-admin users.
            server.disk_size = storage.disk_size_raw;
            server.disk_use = storage.disk_use_raw;
            server.disk_usage_percentage = storage.disk_usage_percentage;
        }

        stats.server = Some(server);
        Ok(stats)
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
    if hex.len() < 4 {
        return originals_dir.join(format!("{hex}.{ext}"));
    }
    originals_dir
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(format!("{hex}.{ext}"))
}

/// Response from `GET /server/statistics` (admin only).
#[derive(Debug, serde::Deserialize)]
struct ServerStatistics {
    usage: i64,
}

/// Response from `GET /assets/statistics`.
#[derive(Debug, serde::Deserialize)]
struct AssetStatistics {
    images: i64,
    videos: i64,
}

/// Response from `GET /server/storage`.
#[derive(Debug, serde::Deserialize)]
struct ServerStorage {
    #[serde(rename = "diskSizeRaw")]
    disk_size_raw: u64,
    #[serde(rename = "diskUseRaw")]
    disk_use_raw: u64,
    #[serde(rename = "diskUsagePercentage")]
    disk_usage_percentage: f64,
}

/// Calculate total disk usage of a directory (recursive).
fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += dir_size(&entry.path());
                }
            }
        }
    }
    total
}

/// Periodic cache eviction loop.
///
/// Waits 30 seconds after startup (to avoid competing with sync/thumbnail
/// downloads), then runs eviction every 10 minutes. Also triggers immediately
/// when the cache limit changes via the watch channel.
const EVICTION_STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
const EVICTION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);

async fn run_cache_evictor(
    originals_dir: PathBuf,
    mut limit_rx: tokio::sync::watch::Receiver<u32>,
) {
    // Delay startup to avoid competing with sync/thumbnail downloads.
    tokio::time::sleep(EVICTION_STARTUP_DELAY).await;
    info!("cache evictor started");

    loop {
        let max_mb = {
            let val = *limit_rx.borrow_and_update();
            val
        };
        evict_originals_cache(&originals_dir, max_mb).await;

        // Wait for the next cycle or a limit change.
        tokio::select! {
            _ = tokio::time::sleep(EVICTION_INTERVAL) => {}
            _ = limit_rx.changed() => {
                debug!("cache limit changed, running eviction immediately");
            }
        }
    }
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

    let (mut entries, total_size) = collect_cache_candidates(originals_dir).await;

    if total_size <= max_bytes {
        debug!(
            total_mb = total_size / (1024 * 1024),
            max_mb,
            files = entries.len(),
            "originals cache within limit"
        );
        return;
    }

    entries.sort_by_key(|(_, _, modified)| *modified);
    delete_oldest_entries(&entries, total_size, max_bytes).await;
}

async fn collect_cache_candidates(
    originals_dir: &std::path::Path,
) -> (Vec<(PathBuf, u64, std::time::SystemTime)>, u64) {
    let mut entries = Vec::new();
    let mut total_size: u64 = 0;

    let Ok(mut read_dir) = tokio::fs::read_dir(originals_dir).await else {
        return (entries, total_size);
    };

    while let Ok(Some(shard1)) = read_dir.next_entry().await {
        if !shard1.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        collect_shard_entries(shard1.path(), &mut entries, &mut total_size).await;
    }

    (entries, total_size)
}

async fn collect_shard_entries(
    shard1_path: PathBuf,
    entries: &mut Vec<(PathBuf, u64, std::time::SystemTime)>,
    total_size: &mut u64,
) {
    let Ok(mut shard1_dir) = tokio::fs::read_dir(&shard1_path).await else {
        return;
    };

    while let Ok(Some(shard2)) = shard1_dir.next_entry().await {
        if !shard2.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        collect_file_entries(shard2.path(), entries, total_size).await;
    }
}

async fn collect_file_entries(
    shard2_path: PathBuf,
    entries: &mut Vec<(PathBuf, u64, std::time::SystemTime)>,
    total_size: &mut u64,
) {
    let Ok(mut shard2_dir) = tokio::fs::read_dir(&shard2_path).await else {
        return;
    };

    while let Ok(Some(file)) = shard2_dir.next_entry().await {
        let Ok(meta) = file.metadata().await else {
            continue;
        };
        if meta.is_file() {
            let size = meta.len();
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            *total_size += size;
            entries.push((file.path(), size, modified));
        }
    }
}

async fn delete_oldest_entries(
    entries: &[(PathBuf, u64, std::time::SystemTime)],
    mut remaining: u64,
    max_bytes: u64,
) {
    let mut evicted_count: u64 = 0;
    let mut evicted_bytes: u64 = 0;

    for (path, size, _) in entries {
        if remaining <= max_bytes {
            break;
        }
        if let Err(e) = tokio::fs::remove_file(path).await {
            tracing::warn!(path = %path.display(), "failed to evict cached original: {e}");
            continue;
        }
        remaining -= size;
        evicted_bytes += size;
        evicted_count += 1;
    }

    info!(
        evicted_files = evicted_count,
        evicted_mb = evicted_bytes / (1024 * 1024),
        remaining_mb = remaining / (1024 * 1024),
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

    async fn albums_containing_media(
        &self,
        media_ids: &[MediaId],
    ) -> Result<std::collections::HashMap<AlbumId, usize>, LibraryError> {
        self.db.albums_containing_media(media_ids).await
    }

    async fn album_cover_media_ids(
        &self,
        album_id: &AlbumId,
        limit: u32,
    ) -> Result<Vec<MediaId>, LibraryError> {
        self.db.album_cover_media_ids(album_id, limit).await
    }
}

#[async_trait]
impl LibraryFaces for ImmichLibrary {
    async fn list_people(
        &self,
        include_hidden: bool,
        include_unnamed: bool,
    ) -> Result<Vec<Person>, LibraryError> {
        self.db.list_people(include_hidden, include_unnamed).await
    }

    async fn list_media_for_person(
        &self,
        person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError> {
        let ids = self.db.list_media_for_person(person_id.as_str()).await?;
        Ok(ids.into_iter().map(MediaId::new).collect())
    }

    async fn rename_person(
        &self,
        person_id: &PersonId,
        name: &str,
    ) -> Result<(), LibraryError> {
        let path = format!("/people/{}", person_id.as_str());
        let body = serde_json::json!({ "name": name });
        self.client.put_no_content(&path, &body).await?;
        self.db.rename_person(person_id.as_str(), name).await
    }

    async fn set_person_hidden(
        &self,
        person_id: &PersonId,
        hidden: bool,
    ) -> Result<(), LibraryError> {
        let path = format!("/people/{}", person_id.as_str());
        let body = serde_json::json!({ "isHidden": hidden });
        self.client.put_no_content(&path, &body).await?;
        self.db.set_person_hidden(person_id.as_str(), hidden).await
    }

    async fn merge_people(
        &self,
        _target: &PersonId,
        _sources: &[PersonId],
    ) -> Result<(), LibraryError> {
        // TODO: wire to Immich API POST /people/{id}/merge (#185)
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

#[async_trait]
impl LibraryEditing for ImmichLibrary {
    async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError> {
        self.db.get_edit_state(id).await
    }

    async fn save_edit_state(&self, id: &MediaId, state: &EditState) -> Result<(), LibraryError> {
        self.db.upsert_edit_state(id, state).await
    }

    async fn revert_edits(&self, id: &MediaId) -> Result<(), LibraryError> {
        // TODO: wire to Immich API to remove edited version (#224)
        self.db.delete_edit_state(id).await
    }

    async fn render_and_save(&self, _id: &MediaId) -> Result<(), LibraryError> {
        // TODO: render edits and upload to Immich (#224)
        Ok(())
    }

    async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.db.has_pending_edits(id).await
    }
}

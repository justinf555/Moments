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
        let job = ImmichImportJob {
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

// ── Immich import (upload) ──────────────────────────────────────────────────

/// Upload job for importing local files to the Immich server.
struct ImmichImportJob {
    client: ImmichClient,
    db: Database,
    events: Sender<LibraryEvent>,
}

impl ImmichImportJob {
    async fn run(&self, sources: Vec<PathBuf>) {
        use crate::library::format::{FormatRegistry, StandardHandler, RawHandler, VideoHandler};
        use crate::library::import::ImportSummary;
        use sha1::Digest;
        use std::sync::Arc;

        let mut registry = FormatRegistry::new();
        registry.register(Arc::new(StandardHandler));
        registry.register(Arc::new(RawHandler));
        registry.register(Arc::new(VideoHandler));

        // Collect all file candidates.
        let mut candidates = Vec::new();
        for source in &sources {
            if source.is_file() {
                candidates.push(source.clone());
            } else if source.is_dir() {
                self.walk_dir(source, &mut candidates);
            }
        }

        let total = candidates.len();
        info!(total, "upload candidates collected");

        let mut summary = ImportSummary::default();
        let now = chrono::Utc::now().timestamp();

        // Insert all candidates into the upload queue.
        for path in &candidates {
            let path_str = path.to_string_lossy();
            let _ = self.db.insert_upload_pending(&path_str, now).await;
        }

        for (idx, path) in candidates.iter().enumerate() {
            let current = idx + 1;
            let _ = self.events.send(LibraryEvent::ImportProgress { current, total });

            match self.upload_one(&registry, path).await {
                Ok(UploadResult::Created) => summary.imported += 1,
                Ok(UploadResult::Duplicate) => summary.skipped_duplicates += 1,
                Ok(UploadResult::Unsupported) => summary.skipped_unsupported += 1,
                Err(e) => {
                    let path_str = path.to_string_lossy();
                    tracing::warn!(path = %path_str, "upload failed: {e}");
                    let _ = self.db.set_upload_status(&path_str, 2, Some(&e.to_string())).await;
                    summary.failed += 1;
                }
            }
        }

        // Clean up completed uploads.
        let _ = self.db.clear_completed_uploads().await;

        let _ = self.events.send(LibraryEvent::ImportComplete(summary));
    }

    async fn upload_one(
        &self,
        formats: &crate::library::format::FormatRegistry,
        source: &std::path::Path,
    ) -> Result<UploadResult, LibraryError> {
        use sha1::Digest;

        let path_str = source.to_string_lossy().to_string();

        // Extension/format check.
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if formats.media_type_with_sniff(source, &ext).is_none() {
            let _ = self.db.set_upload_status(&path_str, 3, None).await;
            return Ok(UploadResult::Unsupported);
        }

        // Compute SHA-1 hash (Immich dedup).
        let source_clone = source.to_path_buf();
        let sha1_hex = tokio::task::spawn_blocking(move || -> Result<String, LibraryError> {
            let data = std::fs::read(&source_clone).map_err(LibraryError::Io)?;
            let hash = sha1::Sha1::digest(&data);
            Ok(format!("{:x}", hash))
        })
        .await
        .map_err(|e| LibraryError::Runtime(e.to_string()))??;

        let _ = self.db.set_upload_hash(&path_str, &sha1_hex).await;

        // Get file created time for the upload.
        let file_created_at = std::fs::metadata(source)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                    .unwrap_or_default()
                    .to_rfc3339()
            })
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        let filename = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload");
        let device_asset_id = format!("{filename}-{sha1_hex}");

        // Upload to Immich.
        let resp = self
            .client
            .upload_asset(source, &device_asset_id, &file_created_at, Some(&sha1_hex))
            .await?;

        if resp.status == "duplicate" {
            let _ = self.db.set_upload_status(&path_str, 3, None).await;
            return Ok(UploadResult::Duplicate);
        }

        // Insert into local cache with server-assigned UUID.
        let media_type = if formats.is_video(&ext) {
            MediaType::Video
        } else {
            MediaType::Image
        };

        let now = chrono::Utc::now().timestamp();
        let record = MediaRecord {
            id: MediaId::new(resp.id.clone()),
            relative_path: format!("immich/{}", resp.id),
            original_filename: filename.to_string(),
            file_size: std::fs::metadata(source).map(|m| m.len() as i64).unwrap_or(0),
            imported_at: now,
            media_type,
            taken_at: None, // Server extracts EXIF
            width: None,
            height: None,
            orientation: 1,
            duration_ms: None,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
        };

        let item = MediaItem {
            id: MediaId::new(resp.id.clone()),
            taken_at: None,
            imported_at: now,
            original_filename: filename.to_string(),
            width: None,
            height: None,
            orientation: 1,
            media_type,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
            duration_ms: None,
        };

        self.db.upsert_media(&record).await?;
        let _ = self.events.send(LibraryEvent::AssetSynced { item });
        let _ = self.db.set_upload_status(&path_str, 1, None).await;

        Ok(UploadResult::Created)
    }

    fn walk_dir(&self, dir: &std::path::Path, candidates: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    self.walk_dir(&path, candidates);
                } else if path.is_file() {
                    candidates.push(path);
                }
            }
        }
    }
}

enum UploadResult {
    Created,
    Duplicate,
    Unsupported,
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

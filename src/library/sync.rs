use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, instrument, warn};

use super::db::Database;
use super::error::LibraryError;
use super::event::LibraryEvent;
use super::immich_client::ImmichClient;
use super::media::{LibraryMedia, MediaId, MediaItem, MediaMetadataRecord, MediaRecord, MediaType};
use super::thumbnail::sharded_thumbnail_path;

/// Handle returned by [`SyncHandle::start`] to signal shutdown.
pub struct SyncHandle {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

/// Interval between sync cycles.
const SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Maximum concurrent thumbnail downloads.
const MAX_THUMBNAIL_WORKERS: usize = 4;
/// Bounded channel capacity for thumbnail download queue.
const THUMBNAIL_QUEUE_SIZE: usize = 1000;

impl SyncHandle {
    /// Spawn the sync manager and thumbnail downloader as background Tokio tasks.
    pub fn start(
        client: ImmichClient,
        db: Database,
        events: Sender<LibraryEvent>,
        thumbnails_dir: PathBuf,
        tokio: tokio::runtime::Handle,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (thumb_tx, thumb_rx) = tokio::sync::mpsc::channel::<MediaId>(THUMBNAIL_QUEUE_SIZE);

        // Spawn the thumbnail downloader.
        let downloader = ThumbnailDownloader {
            client: client.clone(),
            db: db.clone(),
            events: events.clone(),
            thumbnails_dir,
            rx: thumb_rx,
            semaphore: Arc::new(Semaphore::new(MAX_THUMBNAIL_WORKERS)),
        };
        tokio.spawn(async move {
            downloader.run().await;
        });

        // Spawn the sync manager.
        let manager = SyncManager {
            client,
            db,
            events,
            shutdown_rx,
            thumbnail_tx: thumb_tx,
        };

        tokio.spawn(async move {
            if let Err(e) = manager.run().await {
                error!("sync manager error: {e}");
                let _ = manager.events.send(LibraryEvent::Error(e));
            }
        });

        Self { shutdown_tx }
    }

    /// Signal the sync manager to stop.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Background sync engine for the Immich backend.
///
/// Connects to the Immich server via `POST /sync/stream` and upserts
/// assets into the local SQLite cache. See `docs/design-immich-backend.md`.
struct SyncManager {
    client: ImmichClient,
    db: Database,
    events: Sender<LibraryEvent>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    thumbnail_tx: tokio::sync::mpsc::Sender<MediaId>,
}

impl SyncManager {
    /// Main sync loop. Runs an initial sync, then polls every 30 seconds
    /// to pick up changes from mobile uploads and other clients.
    #[instrument(skip(self))]
    async fn run(&self) -> Result<(), LibraryError> {
        info!("sync manager starting");

        loop {
            // Check for shutdown before each cycle.
            if *self.shutdown_rx.borrow() {
                info!("sync manager shutting down");
                break;
            }

            if let Err(e) = self.run_sync().await {
                error!("sync cycle failed: {e}");
                // Don't abort the loop on transient errors — retry next cycle.
            }

            // Wait for the next cycle, but break early on shutdown.
            let mut shutdown = self.shutdown_rx.clone();
            tokio::select! {
                _ = tokio::time::sleep(SYNC_INTERVAL) => {}
                _ = shutdown.changed() => {
                    info!("sync manager shutting down during sleep");
                    break;
                }
            }
        }

        info!("sync manager stopped");
        Ok(())
    }

    /// Execute a single sync cycle against the Immich server.
    #[instrument(skip(self))]
    async fn run_sync(&self) -> Result<(), LibraryError> {
        let request = SyncStreamRequest {
            types: vec![
                "AssetsV1".to_string(),
                "AssetExifsV1".to_string(),
            ],
        };

        debug!("starting sync stream");
        let response = self.client.post_stream("/sync/stream", &request).await?;

        // Read the response as newline-delimited JSON.
        let byte_stream = response
            .bytes_stream()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
        let reader = tokio::io::BufReader::new(
            tokio_util::io::StreamReader::new(byte_stream),
        );

        let mut lines = reader.lines();
        let mut acks: Vec<String> = Vec::new();
        let mut asset_count: usize = 0;
        let mut exif_count: usize = 0;
        let mut delete_count: usize = 0;
        let mut is_reset = false;
        let mut existing_ids: Option<HashSet<String>> = None;
        let mut line_number: usize = 0;

        info!("reading sync stream");

        while let Some(line) = lines.next_line().await.map_err(|e| {
            LibraryError::Immich(format!("failed to read sync stream line {line_number}: {e}"))
        })? {
            line_number += 1;
            if line.is_empty() {
                continue;
            }

            let sync_line: SyncLine = serde_json::from_str(&line).map_err(|e| {
                error!(line_number, line = %line.chars().take(200).collect::<String>(), "failed to parse sync line");
                LibraryError::Immich(format!("failed to parse sync line {line_number}: {e}"))
            })?;

            match sync_line.entity_type.as_str() {
                "SyncResetV1" => {
                    warn!("server requested sync reset — performing full resync");
                    is_reset = true;
                    let ids = self.db.all_media_ids().await?;
                    info!(existing_count = ids.len(), "loaded existing media IDs for reset tracking");
                    existing_ids = Some(ids);
                    self.db.clear_sync_checkpoints().await?;
                    acks.push(sync_line.ack);
                }
                "AssetV1" => {
                    let asset: SyncAssetV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AssetV1: {e}");
                            LibraryError::Immich(format!("invalid AssetV1 at line {line_number}: {e}"))
                        })?;
                    let id = asset.id.clone();
                    self.handle_asset(asset).await?;
                    asset_count += 1;

                    if asset_count % 500 == 0 {
                        info!(assets = asset_count, "sync progress");
                    }

                    // Remove from the reset tracking set if present.
                    if let Some(ref mut ids) = existing_ids {
                        ids.remove(&id);
                    }

                    acks.push(sync_line.ack);
                }
                "AssetDeleteV1" => {
                    let delete: SyncAssetDeleteV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AssetDeleteV1: {e}");
                            LibraryError::Immich(format!("invalid AssetDeleteV1 at line {line_number}: {e}"))
                        })?;

                    debug!(asset_id = %delete.asset_id, "deleting asset from cache");

                    if let Some(ref mut ids) = existing_ids {
                        ids.remove(&delete.asset_id);
                    }

                    self.handle_asset_delete(&delete.asset_id).await?;
                    delete_count += 1;
                    acks.push(sync_line.ack);
                }
                "AssetExifV1" => {
                    let exif: SyncAssetExifV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AssetExifV1: {e}");
                            LibraryError::Immich(format!("invalid AssetExifV1 at line {line_number}: {e}"))
                        })?;
                    self.handle_asset_exif(exif).await?;
                    exif_count += 1;
                    acks.push(sync_line.ack);
                }
                "SyncCompleteV1" => {
                    info!(
                        assets = asset_count,
                        exifs = exif_count,
                        deletes = delete_count,
                        lines = line_number,
                        "sync stream complete"
                    );
                    acks.push(sync_line.ack);
                    break;
                }
                other => {
                    debug!(entity_type = other, line_number, "ignoring unknown sync entity type");
                    acks.push(sync_line.ack);
                }
            }
        }

        // Handle reset: delete assets that weren't in the stream.
        if is_reset {
            if let Some(orphaned_ids) = existing_ids {
                if !orphaned_ids.is_empty() {
                    info!(count = orphaned_ids.len(), "removing orphaned assets after reset sync");
                    let ids: Vec<MediaId> = orphaned_ids
                        .into_iter()
                        .map(MediaId::new)
                        .collect();
                    self.db.delete_permanently(&ids).await?;
                }
            }
        }

        // Acknowledge processed changes.
        if !acks.is_empty() {
            info!(count = acks.len(), "sending sync acks to server");
            let ack_request = SyncAckRequest { acks: acks.clone() };
            if let Err(e) = self.client.post_no_content("/sync/ack", &ack_request).await {
                error!("failed to send sync acks: {e}");
                return Err(e);
            }
            debug!("acks sent successfully");

            // Persist checkpoints locally for delta sync on next launch.
            let mut checkpoints: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for ack in &acks {
                if let Some(entity_type) = ack.split('|').next() {
                    checkpoints.insert(entity_type.to_string(), ack.clone());
                }
            }
            let pairs: Vec<(String, String)> = checkpoints.into_iter().collect();
            info!(count = pairs.len(), "saving sync checkpoints locally");
            self.db.save_sync_checkpoints(&pairs).await?;
        }

        // Log summary — grid updates are already handled per-asset via AssetSynced events.
        if asset_count > 0 {
            info!(count = asset_count, "sync complete — assets synced");
        } else {
            debug!("sync complete — no new assets");
        }

        Ok(())
    }

    /// Upsert an asset from the sync stream into the local cache.
    #[instrument(skip(self, asset), fields(asset_id = %asset.id, filename = %asset.original_file_name))]
    async fn handle_asset(&self, asset: SyncAssetV1) -> Result<(), LibraryError> {
        let media_type = match asset.asset_type.as_str() {
            "VIDEO" => MediaType::Video,
            _ => MediaType::Image,
        };

        let taken_at = parse_datetime(&asset.local_date_time)
            .or_else(|| parse_datetime(&asset.file_created_at));

        let imported_at = parse_datetime(&asset.file_created_at)
            .unwrap_or_else(|| chrono::Utc::now().timestamp());

        let is_trashed = asset.deleted_at.is_some();
        let trashed_at = parse_datetime(&asset.deleted_at);

        let duration_ms = asset.duration.as_deref().and_then(parse_duration_ms);

        let id_str = asset.id.clone();
        let record = MediaRecord {
            id: MediaId::new(asset.id),
            relative_path: format!("immich/{id_str}"), // Placeholder — no local file
            original_filename: asset.original_file_name,
            file_size: 0, // Not in sync DTO
            imported_at,
            media_type,
            taken_at,
            width: asset.width,
            height: asset.height,
            orientation: 1, // Orientation comes from EXIF, handled by AssetExifV1
            duration_ms,
            is_favorite: asset.is_favorite,
            is_trashed,
            trashed_at,
        };

        let media_id = record.id.clone();
        self.db.upsert_media(&record).await?;

        // Emit per-asset event for incremental grid updates.
        let item = MediaItem {
            id: media_id.clone(),
            taken_at,
            imported_at,
            original_filename: record.original_filename.clone(),
            width: record.width,
            height: record.height,
            orientation: record.orientation,
            media_type,
            is_favorite: record.is_favorite,
            is_trashed: record.is_trashed,
            trashed_at: record.trashed_at,
            duration_ms: record.duration_ms,
        };
        let _ = self.events.send(LibraryEvent::AssetSynced { item });

        // Queue thumbnail download — the worker pool handles concurrency.
        self.db.insert_thumbnail_pending(&media_id).await?;
        if self.thumbnail_tx.send(media_id).await.is_err() {
            debug!("thumbnail channel closed, skipping download");
        }

        Ok(())
    }

    /// Upsert EXIF metadata from the sync stream.
    #[instrument(skip(self, exif), fields(asset_id = %exif.asset_id))]
    async fn handle_asset_exif(&self, exif: SyncAssetExifV1) -> Result<(), LibraryError> {
        let record = MediaMetadataRecord {
            media_id: MediaId::new(exif.asset_id),
            camera_make: exif.make,
            camera_model: exif.model,
            lens_model: exif.lens_model,
            aperture: exif.f_number,
            shutter_str: exif.exposure_time,
            iso: exif.iso.map(|v| v as u32),
            focal_length: exif.focal_length,
            gps_lat: exif.latitude,
            gps_lon: exif.longitude,
            gps_alt: None, // Not in sync DTO
            color_space: exif.profile_description,
        };

        self.db.upsert_media_metadata(&record).await?;
        Ok(())
    }

    /// Delete an asset from the local cache.
    #[instrument(skip(self))]
    async fn handle_asset_delete(&self, asset_id: &str) -> Result<(), LibraryError> {
        let id = MediaId::new(asset_id.to_owned());
        self.db.delete_permanently(&[id]).await?;
        Ok(())
    }
}

// ── Thumbnail download worker pool ───────────────────────────────────────────

struct ThumbnailDownloader {
    client: ImmichClient,
    db: Database,
    events: Sender<LibraryEvent>,
    thumbnails_dir: PathBuf,
    rx: tokio::sync::mpsc::Receiver<MediaId>,
    semaphore: Arc<Semaphore>,
}

impl ThumbnailDownloader {
    /// Process thumbnail download requests from the channel.
    ///
    /// Runs until the sender side is dropped (SyncManager finishes or shuts down).
    /// Each download is bounded by the semaphore (max 4 concurrent).
    async fn run(mut self) {
        info!("thumbnail downloader started");
        let mut download_count: usize = 0;

        while let Some(media_id) = self.rx.recv().await {
            let permit = match self.semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break, // semaphore closed
            };

            let client = self.client.clone();
            let db = self.db.clone();
            let events = self.events.clone();
            let thumbnails_dir = self.thumbnails_dir.clone();

            tokio::spawn(async move {
                if let Err(e) = download_thumbnail(
                    &client, &db, &events, &thumbnails_dir, &media_id,
                ).await {
                    debug!(id = %media_id, "thumbnail download failed: {e}");
                }
                drop(permit);
            });

            download_count += 1;
            if download_count % 100 == 0 {
                info!(queued = download_count, "thumbnail download progress");
            }
        }

        info!(total = download_count, "thumbnail downloader finished");
    }
}

/// Download a single thumbnail from Immich and write it to the local cache.
#[instrument(skip(client, db, events, thumbnails_dir))]
async fn download_thumbnail(
    client: &ImmichClient,
    db: &Database,
    events: &Sender<LibraryEvent>,
    thumbnails_dir: &std::path::Path,
    media_id: &MediaId,
) -> Result<(), LibraryError> {
    let path = sharded_thumbnail_path(thumbnails_dir, media_id);

    // Skip if already cached on disk.
    if path.exists() {
        debug!("thumbnail already cached, skipping download");
        let now = chrono::Utc::now().timestamp();
        db.set_thumbnail_ready(media_id, &path.to_string_lossy(), now).await?;
        let _ = events.send(LibraryEvent::ThumbnailReady {
            media_id: media_id.clone(),
        });
        return Ok(());
    }

    // Download from Immich.
    let api_path = format!("/assets/{}/thumbnail?size=thumbnail", media_id.as_str());
    let bytes = client.get_bytes(&api_path).await?;

    // Create shard directories and write file.
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(LibraryError::Io)?;
    }
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(LibraryError::Io)?;

    // Update DB status and emit event.
    let now = chrono::Utc::now().timestamp();
    db.set_thumbnail_ready(media_id, &path.to_string_lossy(), now).await?;
    let _ = events.send(LibraryEvent::ThumbnailReady {
        media_id: media_id.clone(),
    });

    Ok(())
}

// ── Sync protocol types ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SyncLine {
    #[serde(rename = "type")]
    entity_type: String,
    data: serde_json::Value,
    ack: String,
}

#[derive(Debug, Serialize)]
struct SyncStreamRequest {
    types: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SyncAckRequest {
    acks: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SyncAssetV1 {
    id: String,
    #[serde(rename = "originalFileName")]
    original_file_name: String,
    #[serde(rename = "fileCreatedAt")]
    file_created_at: Option<String>,
    #[serde(rename = "localDateTime")]
    local_date_time: Option<String>,
    #[serde(rename = "type")]
    asset_type: String,
    #[serde(rename = "deletedAt")]
    deleted_at: Option<String>,
    #[serde(rename = "isFavorite")]
    is_favorite: bool,
    width: Option<i64>,
    height: Option<i64>,
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SyncAssetDeleteV1 {
    #[serde(rename = "assetId")]
    asset_id: String,
}

#[derive(Debug, Deserialize)]
struct SyncAssetExifV1 {
    #[serde(rename = "assetId")]
    asset_id: String,
    make: Option<String>,
    model: Option<String>,
    #[serde(rename = "lensModel")]
    lens_model: Option<String>,
    #[serde(rename = "fNumber")]
    f_number: Option<f32>,
    #[serde(rename = "exposureTime")]
    exposure_time: Option<String>,
    iso: Option<i64>,
    #[serde(rename = "focalLength")]
    focal_length: Option<f32>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    #[serde(rename = "profileDescription")]
    profile_description: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Parse an ISO 8601 datetime string to Unix timestamp.
fn parse_datetime(s: &Option<String>) -> Option<i64> {
    s.as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
}

/// Parse Immich duration string (e.g. "0:01:30.000000") to milliseconds.
fn parse_duration_ms(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours: u64 = parts[0].parse().ok()?;
    let minutes: u64 = parts[1].parse().ok()?;
    let seconds: f64 = parts[2].parse().ok()?;
    Some(hours * 3_600_000 + minutes * 60_000 + (seconds * 1000.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_datetime_valid() {
        let s = Some("2024-01-15T10:30:00.000Z".to_string());
        let ts = parse_datetime(&s).unwrap();
        assert!(ts > 0);
    }

    #[test]
    fn parse_datetime_none() {
        assert!(parse_datetime(&None).is_none());
    }

    #[test]
    fn parse_datetime_invalid() {
        let s = Some("not-a-date".to_string());
        assert!(parse_datetime(&s).is_none());
    }

    #[test]
    fn parse_duration_ms_valid() {
        assert_eq!(parse_duration_ms("0:01:30.000000"), Some(90_000));
        assert_eq!(parse_duration_ms("1:00:00.000000"), Some(3_600_000));
        assert_eq!(parse_duration_ms("0:00:05.500000"), Some(5_500));
    }

    #[test]
    fn parse_duration_ms_invalid() {
        assert!(parse_duration_ms("invalid").is_none());
        assert!(parse_duration_ms("").is_none());
    }
}

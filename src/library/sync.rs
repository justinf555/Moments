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
use super::db::faces::AssetFaceRow;
use super::error::LibraryError;
use super::album::{AlbumId, LibraryAlbums};
use super::event::LibraryEvent;
use super::immich_client::ImmichClient;
use super::media::{LibraryMedia, MediaId, MediaItem, MediaMetadataRecord, MediaRecord, MediaType};
use super::thumbnail::sharded_thumbnail_path;

/// Handle returned by [`SyncHandle::start`] to signal shutdown.
pub struct SyncHandle {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    interval_tx: tokio::sync::watch::Sender<u64>,
}

/// Maximum concurrent thumbnail downloads.
const MAX_THUMBNAIL_WORKERS: usize = 4;
/// Bounded channel capacity for thumbnail download queue.
const THUMBNAIL_QUEUE_SIZE: usize = 1000;
/// Delay between thumbnail download dispatches to avoid overloading the server.
const THUMBNAIL_THROTTLE: std::time::Duration = std::time::Duration::from_millis(5);
/// Number of acks to accumulate before flushing to server.
const ACK_FLUSH_THRESHOLD: usize = 500;

impl SyncHandle {
    /// Spawn the sync manager and thumbnail downloader as background Tokio tasks.
    ///
    /// `initial_interval_secs` is the polling interval read from GSettings.
    /// Use [`set_interval`] to update it live from the preferences dialog.
    pub fn start(
        client: ImmichClient,
        db: Database,
        events: Sender<LibraryEvent>,
        thumbnails_dir: PathBuf,
        tokio: tokio::runtime::Handle,
        initial_interval_secs: u64,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (interval_tx, interval_rx) = tokio::sync::watch::channel(initial_interval_secs);
        let (thumb_tx, thumb_rx) = tokio::sync::mpsc::channel::<MediaId>(THUMBNAIL_QUEUE_SIZE);

        // Spawn the thumbnail downloader.
        let manager_thumbnails_dir = thumbnails_dir.clone();
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
            thumbnails_dir: manager_thumbnails_dir,
            interval_rx: tokio::sync::Mutex::new(interval_rx),
        };

        tokio.spawn(async move {
            if let Err(e) = manager.run().await {
                error!("sync manager error: {e}");
                let _ = manager.events.send(LibraryEvent::Error(e));
            }
        });

        Self { shutdown_tx, interval_tx }
    }

    /// Signal the sync manager to stop.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Update the polling interval (seconds). Takes effect on the next cycle.
    /// Set to 0 to disable polling (sync on open only).
    pub fn set_interval(&self, secs: u64) {
        let _ = self.interval_tx.send(secs);
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
    thumbnails_dir: PathBuf,
    interval_rx: tokio::sync::Mutex<tokio::sync::watch::Receiver<u64>>,
}

impl SyncManager {
    /// Main sync loop. Runs an initial sync, then polls at the configured
    /// interval. The interval can be updated live via the watch channel.
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

            // Read the current interval (may have changed via preferences).
            let interval_secs: u64 = {
                let mut rx = self.interval_rx.lock().await;
                let val = *rx.borrow_and_update();
                val
            };
            if interval_secs == 0 {
                info!("sync polling disabled (interval=0), stopping after initial sync");
                break;
            }

            let interval = std::time::Duration::from_secs(interval_secs);
            debug!(interval_secs, "waiting for next sync cycle");

            // Wait for the next cycle, but break early on shutdown.
            let mut shutdown = self.shutdown_rx.clone();
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
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
    ///
    /// Records are processed individually with skip-on-failure semantics:
    /// a single failed upsert does not abort the entire cycle. Acks are
    /// flushed to the server every [`ACK_FLUSH_THRESHOLD`] records so that
    /// progress is preserved incrementally.
    #[instrument(skip(self))]
    async fn run_sync(&self) -> Result<(), LibraryError> {
        let request = SyncStreamRequest {
            types: vec![
                "AssetsV1".to_string(),
                "AssetExifsV1".to_string(),
                "AlbumsV1".to_string(),
                "AlbumToAssetsV1".to_string(),
                "PeopleV1".to_string(),
                "AssetFacesV1".to_string(),
            ],
        };

        debug!("starting sync stream");
        let _ = self.events.send(LibraryEvent::SyncStarted);
        let response = self.client.post_stream("/sync/stream", &request).await?;

        let byte_stream = response
            .bytes_stream()
            .map_err(std::io::Error::other);
        let reader = tokio::io::BufReader::new(
            tokio_util::io::StreamReader::new(byte_stream),
        );

        let mut lines = reader.lines();
        let mut acks: Vec<String> = Vec::new();
        let mut asset_count: usize = 0;
        let mut exif_count: usize = 0;
        let mut delete_count: usize = 0;
        let mut person_count: usize = 0;
        let mut face_count: usize = 0;
        let mut album_count: usize = 0;
        let mut error_count: usize = 0;
        let mut is_reset = false;
        let mut existing_ids: Option<HashSet<String>> = None;
        let mut line_number: usize = 0;
        let sync_cycle = chrono::Utc::now().to_rfc3339();

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
                    self.db.clear_asset_faces().await?;
                    self.db.clear_people().await?;
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

                    let audit_id = self.db.start_sync_audit("AssetV1", &id, &sync_cycle).await.ok();

                    match self.handle_asset(asset).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "upsert").await;
                            }
                            acks.push(sync_line.ack);
                            asset_count += 1;
                        }
                        Err(e) => {
                            warn!(asset_id = %id, error = %e, "skipping asset");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                            // Don't ack — server will resend next cycle.
                        }
                    }

                    if asset_count.is_multiple_of(500) && asset_count > 0 {
                        info!(assets = asset_count, "sync progress");
                    }

                    if let Some(ref mut ids) = existing_ids {
                        ids.remove(&id);
                    }
                }
                "AssetDeleteV1" => {
                    let delete: SyncAssetDeleteV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AssetDeleteV1: {e}");
                            LibraryError::Immich(format!("invalid AssetDeleteV1 at line {line_number}: {e}"))
                        })?;

                    let audit_id = self.db.start_sync_audit("AssetDeleteV1", &delete.asset_id, &sync_cycle).await.ok();

                    if let Some(ref mut ids) = existing_ids {
                        ids.remove(&delete.asset_id);
                    }

                    match self.handle_asset_delete(&delete.asset_id).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "delete").await;
                            }
                            acks.push(sync_line.ack);
                            delete_count += 1;
                        }
                        Err(e) => {
                            warn!(asset_id = %delete.asset_id, error = %e, "skipping asset delete");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "AssetExifV1" => {
                    let exif: SyncAssetExifV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AssetExifV1: {e}");
                            LibraryError::Immich(format!("invalid AssetExifV1 at line {line_number}: {e}"))
                        })?;

                    let exif_asset_id = exif.asset_id.clone();
                    let audit_id = self.db.start_sync_audit("AssetExifV1", &exif_asset_id, &sync_cycle).await.ok();

                    match self.handle_asset_exif(exif).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "upsert").await;
                            }
                            acks.push(sync_line.ack);
                            exif_count += 1;
                        }
                        Err(e) => {
                            warn!(asset_id = %exif_asset_id, error = %e, "skipping exif");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "AlbumV1" => {
                    let album: SyncAlbumV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AlbumV1: {e}");
                            LibraryError::Immich(format!("invalid AlbumV1 at line {line_number}: {e}"))
                        })?;

                    let album_id = album.id.clone();
                    let audit_id = self.db.start_sync_audit("AlbumV1", &album_id, &sync_cycle).await.ok();

                    match self.handle_album(album).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "upsert").await;
                            }
                            acks.push(sync_line.ack);
                            album_count += 1;
                        }
                        Err(e) => {
                            warn!(album_id = %album_id, error = %e, "skipping album");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "AlbumDeleteV1" => {
                    let delete: SyncAlbumDeleteV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AlbumDeleteV1: {e}");
                            LibraryError::Immich(format!("invalid AlbumDeleteV1 at line {line_number}: {e}"))
                        })?;

                    let del_album_id = delete.album_id.clone();
                    let audit_id = self.db.start_sync_audit("AlbumDeleteV1", &del_album_id, &sync_cycle).await.ok();

                    match self.handle_album_delete(&delete.album_id).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "delete").await;
                            }
                            acks.push(sync_line.ack);
                        }
                        Err(e) => {
                            warn!(album_id = %del_album_id, error = %e, "skipping album delete");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "AlbumToAssetV1" => {
                    let assoc: SyncAlbumToAssetV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AlbumToAssetV1: {e}");
                            LibraryError::Immich(format!("invalid AlbumToAssetV1 at line {line_number}: {e}"))
                        })?;

                    let assoc_id = format!("{}:{}", assoc.album_id, assoc.asset_id);
                    let audit_id = self.db.start_sync_audit("AlbumToAssetV1", &assoc_id, &sync_cycle).await.ok();

                    match self.handle_album_asset(assoc).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "upsert").await;
                            }
                            acks.push(sync_line.ack);
                        }
                        Err(e) => {
                            warn!(assoc = %assoc_id, error = %e, "skipping album-asset link");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "AlbumToAssetDeleteV1" => {
                    let assoc: SyncAlbumToAssetDeleteV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AlbumToAssetDeleteV1: {e}");
                            LibraryError::Immich(format!("invalid AlbumToAssetDeleteV1 at line {line_number}: {e}"))
                        })?;

                    let assoc_id = format!("{}:{}", assoc.album_id, assoc.asset_id);
                    let audit_id = self.db.start_sync_audit("AlbumToAssetDeleteV1", &assoc_id, &sync_cycle).await.ok();

                    match self.handle_album_asset_delete(assoc).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "delete").await;
                            }
                            acks.push(sync_line.ack);
                        }
                        Err(e) => {
                            warn!(assoc = %assoc_id, error = %e, "skipping album-asset unlink");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "PersonV1" => {
                    let person: SyncPersonV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize PersonV1: {e}");
                            LibraryError::Immich(format!("invalid PersonV1 at line {line_number}: {e}"))
                        })?;
                    let person_id = person.id.clone();
                    let audit_id = self.db.start_sync_audit("PersonV1", &person_id, &sync_cycle).await.ok();

                    match self.handle_person(person).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "upsert").await;
                            }
                            acks.push(sync_line.ack);
                            person_count += 1;
                        }
                        Err(e) => {
                            warn!(person_id = %person_id, error = %e, "skipping person");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "PersonDeleteV1" => {
                    let delete: SyncPersonDeleteV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize PersonDeleteV1: {e}");
                            LibraryError::Immich(format!("invalid PersonDeleteV1 at line {line_number}: {e}"))
                        })?;
                    let person_id = delete.person_id.clone();
                    let audit_id = self.db.start_sync_audit("PersonDeleteV1", &person_id, &sync_cycle).await.ok();

                    match self.db.delete_person(&person_id).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "delete").await;
                            }
                            acks.push(sync_line.ack);
                            delete_count += 1;
                        }
                        Err(e) => {
                            warn!(person_id = %person_id, error = %e, "skipping person delete");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "AssetFaceV1" => {
                    let face: SyncAssetFaceV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AssetFaceV1: {e}");
                            LibraryError::Immich(format!("invalid AssetFaceV1 at line {line_number}: {e}"))
                        })?;
                    let face_id = face.id.clone();
                    let audit_id = self.db.start_sync_audit("AssetFaceV1", &face_id, &sync_cycle).await.ok();

                    match self.handle_asset_face(face).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "upsert").await;
                            }
                            acks.push(sync_line.ack);
                            face_count += 1;
                        }
                        Err(e) => {
                            warn!(face_id = %face_id, error = %e, "skipping asset face");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "AssetFaceDeleteV1" => {
                    let delete: SyncAssetFaceDeleteV1 = serde_json::from_value(sync_line.data)
                        .map_err(|e| {
                            error!(line_number, "failed to deserialize AssetFaceDeleteV1: {e}");
                            LibraryError::Immich(format!("invalid AssetFaceDeleteV1 at line {line_number}: {e}"))
                        })?;
                    let face_id = delete.asset_face_id.clone();
                    let audit_id = self.db.start_sync_audit("AssetFaceDeleteV1", &face_id, &sync_cycle).await.ok();

                    match self.db.delete_asset_face(&face_id).await {
                        Ok(()) => {
                            if let Some(aid) = audit_id {
                                let _ = self.db.complete_sync_audit(aid, "delete").await;
                            }
                            acks.push(sync_line.ack);
                            delete_count += 1;
                        }
                        Err(e) => {
                            warn!(face_id = %face_id, error = %e, "skipping asset face delete");
                            if let Some(aid) = audit_id {
                                let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                            }
                            error_count += 1;
                        }
                    }
                }
                "SyncCompleteV1" => {
                    info!(
                        assets = asset_count,
                        exifs = exif_count,
                        deletes = delete_count,
                        albums = album_count,
                        people = person_count,
                        faces = face_count,
                        errors = error_count,
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

            // Flush acks incrementally so progress is preserved.
            if acks.len() >= ACK_FLUSH_THRESHOLD {
                self.flush_acks(&mut acks).await?;
                let _ = self.events.send(LibraryEvent::SyncProgress {
                    assets: asset_count,
                    people: person_count,
                    faces: face_count,
                });
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

        // Flush any remaining acks.
        if !acks.is_empty() {
            self.flush_acks(&mut acks).await?;
        }

        // Emit sync complete — always, so the status bar reverts to idle.
        let _ = self.events.send(LibraryEvent::SyncComplete {
            assets: asset_count,
            people: person_count,
            faces: face_count,
            errors: error_count,
        });

        // Also emit people-specific event for grid refresh.
        if person_count > 0 || face_count > 0 {
            let _ = self.events.send(LibraryEvent::PeopleSyncComplete);
        }

        if asset_count > 0 || error_count > 0 {
            info!(
                synced = asset_count,
                errors = error_count,
                "sync complete"
            );
        } else {
            debug!("sync complete — no new assets");
        }

        Ok(())
    }

    /// Send accumulated acks to the server and save checkpoints locally.
    ///
    /// Clears `acks` after successful flush. Called both incrementally during
    /// the stream and once at the end for any remaining acks.
    async fn flush_acks(&self, acks: &mut Vec<String>) -> Result<(), LibraryError> {
        if acks.is_empty() {
            return Ok(());
        }

        info!(count = acks.len(), "flushing acks to server");
        for chunk in acks.chunks(1000) {
            let ack_request = SyncAckRequest { acks: chunk.to_vec() };
            self.client.post_no_content("/sync/ack", &ack_request).await?;
        }

        // Persist checkpoints — keep only the latest ack per entity type.
        let mut checkpoints: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for ack in acks.iter() {
            if let Some(entity_type) = ack.split('|').next() {
                checkpoints.insert(entity_type.to_string(), ack.clone());
            }
        }
        let pairs: Vec<(String, String)> = checkpoints.into_iter().collect();
        self.db.save_sync_checkpoints(&pairs).await?;

        acks.clear();
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
        self.db.delete_permanently(std::slice::from_ref(&id)).await?;
        let _ = self.events.send(LibraryEvent::AssetDeletedRemote { media_id: id });
        Ok(())
    }

    /// Upsert an album from the sync stream.
    #[instrument(skip(self, album), fields(album_id = %album.id, name = %album.name))]
    async fn handle_album(&self, album: SyncAlbumV1) -> Result<(), LibraryError> {
        let created_at = parse_datetime(&Some(album.created_at)).unwrap_or(0);
        let updated_at = parse_datetime(&Some(album.updated_at)).unwrap_or(0);

        self.db
            .upsert_album(&album.id, &album.name, created_at, updated_at)
            .await?;

        let _ = self.events.send(LibraryEvent::AlbumCreated {
            id: AlbumId::from_raw(album.id),
            name: album.name,
        });

        Ok(())
    }

    /// Delete an album from the local cache.
    #[instrument(skip(self))]
    async fn handle_album_delete(&self, album_id: &str) -> Result<(), LibraryError> {
        let id = AlbumId::from_raw(album_id.to_owned());
        self.db.delete_album(&id).await?;

        let _ = self.events.send(LibraryEvent::AlbumDeleted { id });

        Ok(())
    }

    /// Add an asset to an album from the sync stream.
    async fn handle_album_asset(&self, assoc: SyncAlbumToAssetV1) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().timestamp();
        self.db
            .upsert_album_media(&assoc.album_id, &assoc.asset_id, now)
            .await?;

        let _ = self.events.send(LibraryEvent::AlbumMediaChanged {
            album_id: AlbumId::from_raw(assoc.album_id),
        });

        Ok(())
    }

    /// Upsert a person from the sync stream and download their face thumbnail.
    #[instrument(skip(self, person), fields(person_id = %person.id, name = %person.name))]
    async fn handle_person(&self, person: SyncPersonV1) -> Result<(), LibraryError> {
        self.db
            .upsert_person(
                &person.id,
                &person.name,
                person.birth_date.as_deref(),
                person.is_hidden,
                person.is_favorite,
                person.color.as_deref(),
                person.face_asset_id.as_deref(),
            )
            .await?;

        // Download person face thumbnail (250×250 JPEG from Immich).
        let person_thumb_dir = self.thumbnails_dir.join("people");
        let thumb_path = person_thumb_dir.join(format!("{}.jpg", person.id));
        if !thumb_path.exists() {
            let api_path = format!("/people/{}/thumbnail", person.id);
            match self.client.get_bytes(&api_path).await {
                Ok(bytes) => {
                    if let Some(parent) = thumb_path.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    let _ = tokio::fs::write(&thumb_path, &bytes).await;
                    debug!(person_id = %person.id, "person thumbnail downloaded");
                }
                Err(e) => {
                    debug!(person_id = %person.id, "person thumbnail download failed: {e}");
                }
            }
        }

        Ok(())
    }

    /// Upsert an asset face from the sync stream and update the person's face count.
    #[instrument(skip(self, face), fields(face_id = %face.id, asset_id = %face.asset_id))]
    async fn handle_asset_face(&self, face: SyncAssetFaceV1) -> Result<(), LibraryError> {
        let row = AssetFaceRow {
            id: face.id,
            asset_id: face.asset_id,
            person_id: face.person_id.clone(),
            image_width: face.image_width,
            image_height: face.image_height,
            bbox_x1: face.bounding_box_x1,
            bbox_y1: face.bounding_box_y1,
            bbox_x2: face.bounding_box_x2,
            bbox_y2: face.bounding_box_y2,
            source_type: face.source_type.unwrap_or_else(|| "MachineLearning".to_string()),
        };

        self.db.upsert_asset_face(&row).await?;

        // Update denormalised face count on the person.
        if let Some(ref person_id) = face.person_id {
            self.db.update_face_count(person_id).await?;
        }

        Ok(())
    }

    /// Remove an asset from an album from the sync stream.
    async fn handle_album_asset_delete(
        &self,
        assoc: SyncAlbumToAssetDeleteV1,
    ) -> Result<(), LibraryError> {
        self.db
            .delete_album_media_entry(&assoc.album_id, &assoc.asset_id)
            .await?;

        let _ = self.events.send(LibraryEvent::AlbumMediaChanged {
            album_id: AlbumId::from_raw(assoc.album_id),
        });

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

            // Emit progress every 10 thumbnails to update the status bar.
            if download_count.is_multiple_of(10) {
                let _ = self.events.send(LibraryEvent::ThumbnailDownloadProgress {
                    completed: download_count,
                    total: download_count, // Total not known upfront; shows running count.
                });
            }

            if download_count.is_multiple_of(100) {
                info!(queued = download_count, "thumbnail download progress");
            }

            // Throttle to avoid overloading the Immich server during bulk syncs.
            tokio::time::sleep(THUMBNAIL_THROTTLE).await;
        }

        let _ = self.events.send(LibraryEvent::ThumbnailDownloadsComplete {
            total: download_count,
        });
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

#[derive(Debug, Deserialize)]
struct SyncAlbumV1 {
    id: String,
    name: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct SyncAlbumDeleteV1 {
    #[serde(rename = "albumId")]
    album_id: String,
}

#[derive(Debug, Deserialize)]
struct SyncAlbumToAssetV1 {
    #[serde(rename = "albumId")]
    album_id: String,
    #[serde(rename = "assetId")]
    asset_id: String,
}

#[derive(Debug, Deserialize)]
struct SyncAlbumToAssetDeleteV1 {
    #[serde(rename = "albumId")]
    album_id: String,
    #[serde(rename = "assetId")]
    asset_id: String,
}

#[derive(Debug, Deserialize)]
struct SyncPersonV1 {
    id: String,
    name: String,
    #[serde(rename = "birthDate")]
    birth_date: Option<String>,
    #[serde(rename = "isHidden")]
    is_hidden: bool,
    #[serde(rename = "isFavorite")]
    is_favorite: bool,
    color: Option<String>,
    #[serde(rename = "faceAssetId")]
    face_asset_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SyncPersonDeleteV1 {
    #[serde(rename = "personId")]
    person_id: String,
}

#[derive(Debug, Deserialize)]
struct SyncAssetFaceV1 {
    id: String,
    #[serde(rename = "assetId")]
    asset_id: String,
    #[serde(rename = "personId")]
    person_id: Option<String>,
    #[serde(rename = "imageWidth")]
    image_width: i32,
    #[serde(rename = "imageHeight")]
    image_height: i32,
    #[serde(rename = "boundingBoxX1")]
    bounding_box_x1: i32,
    #[serde(rename = "boundingBoxY1")]
    bounding_box_y1: i32,
    #[serde(rename = "boundingBoxX2")]
    bounding_box_x2: i32,
    #[serde(rename = "boundingBoxY2")]
    bounding_box_y2: i32,
    #[serde(rename = "sourceType")]
    source_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SyncAssetFaceDeleteV1 {
    #[serde(rename = "assetFaceId")]
    asset_face_id: String,
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
    use crate::library::db::test_helpers::open_test_db;
    use tempfile::tempdir;

    /// Create a SyncManager with a real test DB for handler tests.
    /// The ImmichClient points to a dummy URL — only tests that don't
    /// call HTTP methods (handle_asset, handle_album, etc.) are safe.
    async fn test_sync_manager(
        db: Database,
    ) -> (SyncManager, std::sync::mpsc::Receiver<LibraryEvent>) {
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (thumbnail_tx, _thumbnail_rx) = tokio::sync::mpsc::channel(100);
        let (interval_tx, interval_rx) = tokio::sync::watch::channel(60u64);
        // Keep senders alive so channels don't close.
        std::mem::forget(shutdown_tx);
        std::mem::forget(interval_tx);

        let client = ImmichClient::new("http://localhost:9999", "test-token").unwrap();
        let manager = SyncManager {
            client,
            db,
            events: event_tx,
            shutdown_rx,
            thumbnail_tx,
            thumbnails_dir: PathBuf::from("/tmp/test-thumbnails"),
            interval_rx: tokio::sync::Mutex::new(interval_rx),
        };
        (manager, event_rx)
    }

    // ── Helper function tests ───────────────────────────────────────────

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

    // ── handle_asset tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn handle_asset_upserts_image() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let (mgr, events) = test_sync_manager(db.clone()).await;

        let asset = SyncAssetV1 {
            id: "asset-001".to_string(),
            original_file_name: "sunset.jpg".to_string(),
            asset_type: "IMAGE".to_string(),
            is_favorite: true,
            deleted_at: None,
            file_created_at: Some("2024-06-15T12:00:00.000Z".to_string()),
            local_date_time: Some("2024-06-15T14:00:00.000+02:00".to_string()),
            duration: None,
            width: Some(4032),
            height: Some(3024),
        };

        mgr.handle_asset(asset).await.unwrap();

        // Verify DB state.
        let id = MediaId::new("asset-001".to_string());
        assert!(db.media_exists(&id).await.unwrap());
        let item = db.get_media_item(&id).await.unwrap().unwrap();
        assert_eq!(item.original_filename, "sunset.jpg");
        assert!(item.is_favorite);
        assert!(!item.is_trashed);
        assert_eq!(item.width, Some(4032));
        assert_eq!(item.height, Some(3024));
        assert_eq!(item.media_type, MediaType::Image);
        assert!(item.taken_at.is_some());

        // Verify event emitted.
        let event = events.try_recv().unwrap();
        assert!(matches!(event, LibraryEvent::AssetSynced { .. }));
    }

    #[tokio::test]
    async fn handle_asset_upserts_video_with_duration() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let (mgr, _events) = test_sync_manager(db.clone()).await;

        let asset = SyncAssetV1 {
            id: "video-001".to_string(),
            original_file_name: "clip.mp4".to_string(),
            asset_type: "VIDEO".to_string(),
            is_favorite: false,
            deleted_at: None,
            file_created_at: Some("2024-03-01T08:00:00.000Z".to_string()),
            local_date_time: None,
            duration: Some("0:01:30.000000".to_string()),
            width: Some(1920),
            height: Some(1080),
        };

        mgr.handle_asset(asset).await.unwrap();

        let id = MediaId::new("video-001".to_string());
        let item = db.get_media_item(&id).await.unwrap().unwrap();
        assert_eq!(item.media_type, MediaType::Video);
        assert_eq!(item.duration_ms, Some(90_000));
    }

    #[tokio::test]
    async fn handle_asset_trashed_item() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let (mgr, _events) = test_sync_manager(db.clone()).await;

        let asset = SyncAssetV1 {
            id: "trashed-001".to_string(),
            original_file_name: "deleted.jpg".to_string(),
            asset_type: "IMAGE".to_string(),
            is_favorite: false,
            deleted_at: Some("2024-07-01T00:00:00.000Z".to_string()),
            file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
            local_date_time: None,
            duration: None,
            width: None,
            height: None,
        };

        mgr.handle_asset(asset).await.unwrap();

        let id = MediaId::new("trashed-001".to_string());
        let item = db.get_media_item(&id).await.unwrap().unwrap();
        assert!(item.is_trashed);
        assert!(item.trashed_at.is_some());
    }

    // ── handle_asset_exif tests ─────────────────────────────────────────

    #[tokio::test]
    async fn handle_asset_exif_upserts_metadata() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let (mgr, _events) = test_sync_manager(db.clone()).await;

        // First insert the asset so the FK exists.
        let asset = SyncAssetV1 {
            id: "exif-asset".to_string(),
            original_file_name: "photo.jpg".to_string(),
            asset_type: "IMAGE".to_string(),
            is_favorite: false,
            deleted_at: None,
            file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
            local_date_time: None,
            duration: None,
            width: Some(4000),
            height: Some(3000),
        };
        mgr.handle_asset(asset).await.unwrap();

        let exif = SyncAssetExifV1 {
            asset_id: "exif-asset".to_string(),
            make: Some("Canon".to_string()),
            model: Some("EOS R5".to_string()),
            lens_model: Some("RF 24-70mm F2.8".to_string()),
            f_number: Some(2.8),
            exposure_time: Some("1/250".to_string()),
            iso: Some(400),
            focal_length: Some(50.0),
            latitude: Some(51.5074),
            longitude: Some(-0.1278),
            profile_description: Some("sRGB".to_string()),
        };

        // Should succeed without error — metadata is stored in the DB.
        mgr.handle_asset_exif(exif).await.unwrap();
    }

    // ── handle_album tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn handle_album_upserts_album() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let (mgr, events) = test_sync_manager(db.clone()).await;

        let album = SyncAlbumV1 {
            id: "album-001".to_string(),
            name: "Holiday 2024".to_string(),
            created_at: "2024-06-01T00:00:00.000Z".to_string(),
            updated_at: "2024-06-15T00:00:00.000Z".to_string(),
        };

        mgr.handle_album(album).await.unwrap();

        let albums = db.list_albums().await.unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name, "Holiday 2024");
        assert_eq!(albums[0].id.as_str(), "album-001");

        let event = events.try_recv().unwrap();
        assert!(matches!(event, LibraryEvent::AlbumCreated { .. }));
    }

    #[tokio::test]
    async fn handle_album_delete_removes_album() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let (mgr, _events) = test_sync_manager(db.clone()).await;

        // Create then delete.
        let album = SyncAlbumV1 {
            id: "album-del".to_string(),
            name: "To Delete".to_string(),
            created_at: "2024-01-01T00:00:00.000Z".to_string(),
            updated_at: "2024-01-01T00:00:00.000Z".to_string(),
        };
        mgr.handle_album(album).await.unwrap();
        assert_eq!(db.list_albums().await.unwrap().len(), 1);

        mgr.handle_album_delete("album-del").await.unwrap();
        assert!(db.list_albums().await.unwrap().is_empty());
    }

    // ── handle_asset_face tests ─────────────────────────────────────────

    #[tokio::test]
    async fn handle_asset_face_upserts_face_and_updates_count() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let (mgr, _events) = test_sync_manager(db.clone()).await;

        // Create the asset first (FK constraint).
        let asset = SyncAssetV1 {
            id: "face-asset".to_string(),
            original_file_name: "portrait.jpg".to_string(),
            asset_type: "IMAGE".to_string(),
            is_favorite: false,
            deleted_at: None,
            file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
            local_date_time: None,
            duration: None,
            width: Some(4000),
            height: Some(3000),
        };
        mgr.handle_asset(asset).await.unwrap();

        // Create the person.
        db.upsert_person("person-001", "Alice", None, false, false, None, None)
            .await
            .unwrap();

        let face = SyncAssetFaceV1 {
            id: "face-001".to_string(),
            asset_id: "face-asset".to_string(),
            person_id: Some("person-001".to_string()),
            image_width: 4000,
            image_height: 3000,
            bounding_box_x1: 100,
            bounding_box_y1: 200,
            bounding_box_x2: 300,
            bounding_box_y2: 400,
            source_type: Some("MachineLearning".to_string()),
        };

        mgr.handle_asset_face(face).await.unwrap();

        // Verify face count on person was updated.
        let people = db.list_people(false, false).await.unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].face_count, 1);
    }

    // ── handle_album_asset tests ────────────────────────────────────────

    #[tokio::test]
    async fn handle_album_asset_links_media_to_album() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let (mgr, _events) = test_sync_manager(db.clone()).await;

        // Create album and asset.
        let album = SyncAlbumV1 {
            id: "link-album".to_string(),
            name: "Linked".to_string(),
            created_at: "2024-01-01T00:00:00.000Z".to_string(),
            updated_at: "2024-01-01T00:00:00.000Z".to_string(),
        };
        mgr.handle_album(album).await.unwrap();

        let asset = SyncAssetV1 {
            id: "link-asset".to_string(),
            original_file_name: "linked.jpg".to_string(),
            asset_type: "IMAGE".to_string(),
            is_favorite: false,
            deleted_at: None,
            file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
            local_date_time: None,
            duration: None,
            width: None,
            height: None,
        };
        mgr.handle_asset(asset).await.unwrap();

        let assoc = SyncAlbumToAssetV1 {
            album_id: "link-album".to_string(),
            asset_id: "link-asset".to_string(),
        };
        mgr.handle_album_asset(assoc).await.unwrap();

        let aid = AlbumId::from_raw("link-album".to_string());
        let items = db.list_album_media(&aid, None, 50).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_str(), "link-asset");
    }

    // ── handle_asset_delete tests ───────────────────────────────────────

    #[tokio::test]
    async fn handle_asset_delete_removes_asset() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let (mgr, events) = test_sync_manager(db.clone()).await;

        // Create then delete.
        let asset = SyncAssetV1 {
            id: "del-asset".to_string(),
            original_file_name: "gone.jpg".to_string(),
            asset_type: "IMAGE".to_string(),
            is_favorite: false,
            deleted_at: None,
            file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
            local_date_time: None,
            duration: None,
            width: None,
            height: None,
        };
        mgr.handle_asset(asset).await.unwrap();
        // Drain the AssetSynced event.
        let _ = events.try_recv();

        let id = MediaId::new("del-asset".to_string());
        assert!(db.media_exists(&id).await.unwrap());

        mgr.handle_asset_delete("del-asset").await.unwrap();
        assert!(!db.media_exists(&id).await.unwrap());

        let event = events.try_recv().unwrap();
        assert!(matches!(event, LibraryEvent::AssetDeletedRemote { .. }));
    }
}

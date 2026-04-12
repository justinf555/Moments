use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use futures_util::TryStreamExt;
use tokio::io::AsyncBufReadExt;
use tracing::{debug, error, info, instrument, warn};

use super::super::album::{AlbumId, LibraryAlbums};
use super::super::db::faces::AssetFaceRow;
use super::super::db::Database;
use super::super::error::LibraryError;
use super::super::event::LibraryEvent;
use super::super::immich_client::ImmichClient;
use super::super::media::{
    LibraryMedia, MediaId, MediaItem, MediaMetadataRecord, MediaRecord, MediaType,
};
use super::types::*;
use super::SyncCounters;
use super::ACK_FLUSH_THRESHOLD;

/// Background sync engine for the Immich backend.
///
/// Connects to the Immich server via `POST /sync/stream` and upserts
/// assets into the local SQLite cache. See `docs/design-immich-backend.md`.
pub(crate) struct SyncManager {
    pub client: ImmichClient,
    pub db: Database,
    /// Event channel to the GTK idle loop. Sends use `let _ =` because
    /// the receiver may be dropped during app shutdown — this is intentional.
    pub events: Sender<LibraryEvent>,
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
    pub thumbnail_tx: tokio::sync::mpsc::Sender<MediaId>,
    pub thumbnails_dir: PathBuf,
    pub interval_rx: tokio::sync::Mutex<tokio::sync::watch::Receiver<u64>>,
}

impl SyncManager {
    /// Main sync loop. Runs an initial sync, then polls at the configured
    /// interval. The interval can be updated live via the watch channel.
    #[instrument(skip(self))]
    pub async fn run(&self) -> Result<(), LibraryError> {
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
        // Receiver may be dropped during shutdown.
        let _ = self.events.send(LibraryEvent::SyncStarted);
        let response = self.client.post_stream("/sync/stream", &request).await?;

        let byte_stream = response.bytes_stream().map_err(std::io::Error::other);
        let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(byte_stream));

        let mut lines = reader.lines();
        let mut acks: Vec<String> = Vec::new();
        let mut counters = SyncCounters::default();
        let mut is_reset = false;
        let mut existing_ids: Option<HashSet<String>> = None;
        let mut line_number: usize = 0;
        let sync_cycle = chrono::Utc::now().to_rfc3339();

        info!("reading sync stream");

        while let Some(line) = lines.next_line().await.map_err(|e| {
            LibraryError::Immich(format!(
                "failed to read sync stream line {line_number}: {e}"
            ))
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
                    self.handle_sync_reset(&mut is_reset, &mut existing_ids)
                        .await?;
                    acks.push(sync_line.ack);
                }
                "AssetV1" => {
                    let asset: SyncAssetV1 =
                        deserialize_entity(&sync_line.data, "AssetV1", line_number)?;
                    let id = asset.id.clone();
                    self.process_entity(
                        "AssetV1",
                        &id,
                        &sync_cycle,
                        "upsert",
                        sync_line.ack,
                        self.handle_asset(asset),
                        &mut acks,
                        &mut counters.assets,
                        &mut counters.errors,
                    )
                    .await;
                    if counters.assets.is_multiple_of(500) && counters.assets > 0 {
                        info!(assets = counters.assets, "sync progress");
                    }
                    if let Some(ref mut ids) = existing_ids {
                        ids.remove(&id);
                    }
                }
                "AssetDeleteV1" => {
                    let delete: SyncAssetDeleteV1 =
                        deserialize_entity(&sync_line.data, "AssetDeleteV1", line_number)?;
                    let id = delete.asset_id.clone();
                    if let Some(ref mut ids) = existing_ids {
                        ids.remove(&id);
                    }
                    self.process_entity(
                        "AssetDeleteV1",
                        &id,
                        &sync_cycle,
                        "delete",
                        sync_line.ack,
                        self.handle_asset_delete(&id),
                        &mut acks,
                        &mut counters.deletes,
                        &mut counters.errors,
                    )
                    .await;
                }
                "AssetExifV1" => {
                    let exif: SyncAssetExifV1 =
                        deserialize_entity(&sync_line.data, "AssetExifV1", line_number)?;
                    let id = exif.asset_id.clone();
                    self.process_entity(
                        "AssetExifV1",
                        &id,
                        &sync_cycle,
                        "upsert",
                        sync_line.ack,
                        self.handle_asset_exif(exif),
                        &mut acks,
                        &mut counters.exifs,
                        &mut counters.errors,
                    )
                    .await;
                }
                "AlbumV1" => {
                    let album: SyncAlbumV1 =
                        deserialize_entity(&sync_line.data, "AlbumV1", line_number)?;
                    let id = album.id.clone();
                    self.process_entity(
                        "AlbumV1",
                        &id,
                        &sync_cycle,
                        "upsert",
                        sync_line.ack,
                        self.handle_album(album),
                        &mut acks,
                        &mut counters.albums,
                        &mut counters.errors,
                    )
                    .await;
                }
                "AlbumDeleteV1" => {
                    let delete: SyncAlbumDeleteV1 =
                        deserialize_entity(&sync_line.data, "AlbumDeleteV1", line_number)?;
                    let id = delete.album_id.clone();
                    self.process_entity(
                        "AlbumDeleteV1",
                        &id,
                        &sync_cycle,
                        "delete",
                        sync_line.ack,
                        self.handle_album_delete(&id),
                        &mut acks,
                        &mut counters.deletes,
                        &mut counters.errors,
                    )
                    .await;
                }
                "AlbumToAssetV1" => {
                    let assoc: SyncAlbumToAssetV1 =
                        deserialize_entity(&sync_line.data, "AlbumToAssetV1", line_number)?;
                    let id = format!("{}:{}", assoc.album_id, assoc.asset_id);
                    self.process_entity(
                        "AlbumToAssetV1",
                        &id,
                        &sync_cycle,
                        "upsert",
                        sync_line.ack,
                        self.handle_album_asset(assoc),
                        &mut acks,
                        &mut counters.albums,
                        &mut counters.errors,
                    )
                    .await;
                }
                "AlbumToAssetDeleteV1" => {
                    let assoc: SyncAlbumToAssetDeleteV1 =
                        deserialize_entity(&sync_line.data, "AlbumToAssetDeleteV1", line_number)?;
                    let id = format!("{}:{}", assoc.album_id, assoc.asset_id);
                    self.process_entity(
                        "AlbumToAssetDeleteV1",
                        &id,
                        &sync_cycle,
                        "delete",
                        sync_line.ack,
                        self.handle_album_asset_delete(assoc),
                        &mut acks,
                        &mut counters.albums,
                        &mut counters.errors,
                    )
                    .await;
                }
                "PersonV1" => {
                    let person: SyncPersonV1 =
                        deserialize_entity(&sync_line.data, "PersonV1", line_number)?;
                    let id = person.id.clone();
                    self.process_entity(
                        "PersonV1",
                        &id,
                        &sync_cycle,
                        "upsert",
                        sync_line.ack,
                        self.handle_person(person),
                        &mut acks,
                        &mut counters.people,
                        &mut counters.errors,
                    )
                    .await;
                }
                "PersonDeleteV1" => {
                    let delete: SyncPersonDeleteV1 =
                        deserialize_entity(&sync_line.data, "PersonDeleteV1", line_number)?;
                    let id = delete.person_id.clone();
                    self.process_entity(
                        "PersonDeleteV1",
                        &id,
                        &sync_cycle,
                        "delete",
                        sync_line.ack,
                        self.db.delete_person(&id),
                        &mut acks,
                        &mut counters.deletes,
                        &mut counters.errors,
                    )
                    .await;
                }
                "AssetFaceV1" => {
                    let face: SyncAssetFaceV1 =
                        deserialize_entity(&sync_line.data, "AssetFaceV1", line_number)?;
                    let id = face.id.clone();
                    self.process_entity(
                        "AssetFaceV1",
                        &id,
                        &sync_cycle,
                        "upsert",
                        sync_line.ack,
                        self.handle_asset_face(face),
                        &mut acks,
                        &mut counters.faces,
                        &mut counters.errors,
                    )
                    .await;
                }
                "AssetFaceDeleteV1" => {
                    let delete: SyncAssetFaceDeleteV1 =
                        deserialize_entity(&sync_line.data, "AssetFaceDeleteV1", line_number)?;
                    let id = delete.asset_face_id.clone();
                    self.process_entity(
                        "AssetFaceDeleteV1",
                        &id,
                        &sync_cycle,
                        "delete",
                        sync_line.ack,
                        self.db.delete_asset_face(&id),
                        &mut acks,
                        &mut counters.deletes,
                        &mut counters.errors,
                    )
                    .await;
                }
                "SyncCompleteV1" => {
                    info!(
                        assets = counters.assets,
                        exifs = counters.exifs,
                        deletes = counters.deletes,
                        albums = counters.albums,
                        people = counters.people,
                        faces = counters.faces,
                        errors = counters.errors,
                        lines = line_number,
                        "sync stream complete"
                    );
                    acks.push(sync_line.ack);
                    break;
                }
                other => {
                    debug!(
                        entity_type = other,
                        line_number, "ignoring unknown sync entity type"
                    );
                    acks.push(sync_line.ack);
                }
            }

            if acks.len() >= ACK_FLUSH_THRESHOLD {
                self.flush_acks(&mut acks).await?;
                // Receiver may be dropped during shutdown.
                let _ = self.events.send(LibraryEvent::SyncProgress {
                    assets: counters.assets,
                    people: counters.people,
                    faces: counters.faces,
                });
            }
        }

        self.finish_sync(is_reset, existing_ids, &mut acks, &counters)
            .await
    }

    pub(crate) async fn handle_sync_reset(
        &self,
        is_reset: &mut bool,
        existing_ids: &mut Option<HashSet<String>>,
    ) -> Result<(), LibraryError> {
        warn!("server requested sync reset — performing full resync");
        *is_reset = true;
        let ids = self.db.all_media_ids().await?;
        info!(
            existing_count = ids.len(),
            "loaded existing media IDs for reset tracking"
        );
        *existing_ids = Some(ids);
        self.db.clear_asset_faces().await?;
        self.db.clear_people().await?;
        self.db.clear_sync_checkpoints().await?;
        Ok(())
    }

    /// Process a single sync entity: audit, run handler, ack on success or
    /// log + increment error count on failure.
    ///
    /// On error the ack is not sent — the server will resend next cycle.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn process_entity(
        &self,
        entity_type: &str,
        entity_id: &str,
        sync_cycle: &str,
        audit_action: &str,
        ack: String,
        handler_result: impl std::future::Future<Output = Result<(), LibraryError>>,
        acks: &mut Vec<String>,
        success_counter: &mut usize,
        error_counter: &mut usize,
    ) {
        // Audit trail is best-effort — failure shouldn't block sync processing.
        let audit_id = self
            .db
            .start_sync_audit(entity_type, entity_id, sync_cycle)
            .await
            .ok();

        match handler_result.await {
            Ok(()) => {
                if let Some(aid) = audit_id {
                    // Audit completion is best-effort.
                    let _ = self.db.complete_sync_audit(aid, audit_action).await;
                }
                acks.push(ack);
                *success_counter += 1;
            }
            Err(e) => {
                warn!(entity_type, entity_id, error = %e, "skipping sync entity");
                if let Some(aid) = audit_id {
                    // Audit failure recording is best-effort.
                    let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                }
                *error_counter += 1;
            }
        }
    }

    pub(crate) async fn finish_sync(
        &self,
        is_reset: bool,
        existing_ids: Option<HashSet<String>>,
        acks: &mut Vec<String>,
        counters: &SyncCounters,
    ) -> Result<(), LibraryError> {
        if is_reset {
            if let Some(orphaned_ids) = existing_ids {
                if !orphaned_ids.is_empty() {
                    info!(
                        count = orphaned_ids.len(),
                        "removing orphaned assets after reset sync"
                    );
                    let ids: Vec<MediaId> = orphaned_ids.into_iter().map(MediaId::new).collect();
                    self.db.delete_permanently(&ids).await?;
                }
            }
        }

        if !acks.is_empty() {
            self.flush_acks(acks).await?;
        }

        // Receiver may be dropped during shutdown.
        let _ = self.events.send(LibraryEvent::SyncComplete {
            assets: counters.assets,
            people: counters.people,
            faces: counters.faces,
            errors: counters.errors,
        });

        if counters.people > 0 || counters.faces > 0 {
            // Receiver may be dropped during shutdown.
            let _ = self.events.send(LibraryEvent::PeopleSyncComplete);
        }

        if counters.assets > 0 || counters.errors > 0 {
            info!(
                synced = counters.assets,
                errors = counters.errors,
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
            let ack_request = SyncAckRequest {
                acks: chunk.to_vec(),
            };
            self.client
                .post_no_content("/sync/ack", &ack_request)
                .await?;
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
    pub(crate) async fn handle_asset(&self, asset: SyncAssetV1) -> Result<(), LibraryError> {
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
        // Receiver may be dropped during shutdown.
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
    pub(crate) async fn handle_asset_exif(
        &self,
        exif: SyncAssetExifV1,
    ) -> Result<(), LibraryError> {
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
    pub(crate) async fn handle_asset_delete(&self, asset_id: &str) -> Result<(), LibraryError> {
        let id = MediaId::new(asset_id.to_owned());
        self.db
            .delete_permanently(std::slice::from_ref(&id))
            .await?;
        // Receiver may be dropped during shutdown.
        let _ = self
            .events
            .send(LibraryEvent::AssetDeletedRemote { media_id: id });
        Ok(())
    }

    /// Upsert an album from the sync stream.
    #[instrument(skip(self, album), fields(album_id = %album.id, name = %album.name))]
    pub(crate) async fn handle_album(&self, album: SyncAlbumV1) -> Result<(), LibraryError> {
        let created_at = parse_datetime(&Some(album.created_at)).unwrap_or(0);
        let updated_at = parse_datetime(&Some(album.updated_at)).unwrap_or(0);

        self.db
            .upsert_album(&album.id, &album.name, created_at, updated_at)
            .await?;

        // Receiver may be dropped during shutdown.
        let _ = self.events.send(LibraryEvent::AlbumCreated {
            id: AlbumId::from_raw(album.id),
            name: album.name,
        });

        Ok(())
    }

    /// Delete an album from the local cache.
    #[instrument(skip(self))]
    pub(crate) async fn handle_album_delete(&self, album_id: &str) -> Result<(), LibraryError> {
        let id = AlbumId::from_raw(album_id.to_owned());
        self.db.delete_album(&id).await?;

        // Receiver may be dropped during shutdown.
        let _ = self.events.send(LibraryEvent::AlbumDeleted { id });

        Ok(())
    }

    /// Add an asset to an album from the sync stream.
    pub(crate) async fn handle_album_asset(
        &self,
        assoc: SyncAlbumToAssetV1,
    ) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().timestamp();
        self.db
            .upsert_album_media(&assoc.album_id, &assoc.asset_id, now)
            .await?;

        // Receiver may be dropped during shutdown.
        let _ = self.events.send(LibraryEvent::AlbumMediaChanged {
            album_id: AlbumId::from_raw(assoc.album_id),
        });

        Ok(())
    }

    /// Upsert a person from the sync stream and download their face thumbnail.
    #[instrument(skip(self, person), fields(person_id = %person.id, name = %person.name))]
    pub(crate) async fn handle_person(&self, person: SyncPersonV1) -> Result<(), LibraryError> {
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
                        // Best-effort: dir may already exist.
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    // Best-effort: person thumbnail is non-critical.
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
    pub(crate) async fn handle_asset_face(
        &self,
        face: SyncAssetFaceV1,
    ) -> Result<(), LibraryError> {
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
            source_type: face
                .source_type
                .unwrap_or_else(|| "MachineLearning".to_string()),
        };

        self.db.upsert_asset_face(&row).await?;

        // Update denormalised face count on the person.
        if let Some(ref person_id) = face.person_id {
            self.db.update_face_count(person_id).await?;
        }

        Ok(())
    }

    /// Remove an asset from an album from the sync stream.
    pub(crate) async fn handle_album_asset_delete(
        &self,
        assoc: SyncAlbumToAssetDeleteV1,
    ) -> Result<(), LibraryError> {
        self.db
            .delete_album_media_entry(&assoc.album_id, &assoc.asset_id)
            .await?;

        // Receiver may be dropped during shutdown.
        let _ = self.events.send(LibraryEvent::AlbumMediaChanged {
            album_id: AlbumId::from_raw(assoc.album_id),
        });

        Ok(())
    }
}

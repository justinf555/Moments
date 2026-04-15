//! Pull sync manager — streams changes from Immich and upserts locally.
//!
//! Connects to `POST /sync/stream`, processes NDJSON entity records,
//! and flushes acks incrementally. See `docs/design-immich-backend.md`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::TryStreamExt;
use tokio::io::AsyncBufReadExt;
use tracing::{debug, error, info, instrument, warn};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::album::AlbumId;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::faces::repository::AssetFaceRow;
use crate::library::media::{MediaId, MediaItem, MediaRecord, MediaType};
use crate::library::metadata::MediaMetadataRecord;
use crate::library::Library;

use super::client::ImmichClient;
use super::types::*;
use super::ACK_FLUSH_THRESHOLD;

/// Counters for a single sync cycle.
#[derive(Default)]
struct SyncCounters {
    assets: usize,
    exifs: usize,
    deletes: usize,
    albums: usize,
    people: usize,
    faces: usize,
    errors: usize,
}

/// Background pull sync engine for the Immich backend.
pub(crate) struct PullManager {
    pub client: ImmichClient,
    pub library: Arc<Library>,
    /// Database handle for sync infrastructure (checkpoints, audit).
    pub db: Database,
    pub events: EventSender,
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
    pub thumbnail_tx: tokio::sync::mpsc::Sender<MediaId>,
    pub thumbnails_dir: PathBuf,
    pub interval_rx: tokio::sync::Mutex<tokio::sync::watch::Receiver<u64>>,
}

impl PullManager {
    /// Main sync loop. Runs an initial sync, then polls at the configured
    /// interval. The interval can be updated live via the watch channel.
    #[instrument(skip(self))]
    pub async fn run(&self) -> Result<(), LibraryError> {
        info!("pull manager starting");

        loop {
            if *self.shutdown_rx.borrow() {
                info!("pull manager shutting down");
                break;
            }

            if let Err(e) = self.run_sync().await {
                error!("sync cycle failed: {e}");
            }

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

            let mut shutdown = self.shutdown_rx.clone();
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = shutdown.changed() => {
                    info!("pull manager shutting down during sleep");
                    break;
                }
            }
        }

        info!("pull manager stopped");
        Ok(())
    }

    /// Execute a single sync cycle.
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
        self.events.send(AppEvent::SyncStarted);
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
                error!(
                    line_number,
                    line = %line.chars().take(200).collect::<String>(),
                    "failed to parse sync line"
                );
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
                    if counters.assets % 500 == 0 && counters.assets > 0 {
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
                    let assoc: SyncAlbumToAssetDeleteV1 = deserialize_entity(
                        &sync_line.data,
                        "AlbumToAssetDeleteV1",
                        line_number,
                    )?;
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
                        self.library.faces().delete_person_by_id(&id),
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
                        self.library.faces().delete_asset_face(&id),
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
                self.events.send(AppEvent::SyncProgress {
                    assets: counters.assets,
                    people: counters.people,
                    faces: counters.faces,
                });
            }
        }

        self.finish_sync(is_reset, existing_ids, &mut acks, &counters)
            .await
    }

    // ── Entity handlers ─────────────────────────────────────────────────

    #[instrument(skip(self, asset), fields(asset_id = %asset.id))]
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
            id: MediaId::new(id_str.clone()),
            content_hash: None,
            external_id: Some(id_str.clone()),
            relative_path: format!("immich/{id_str}"),
            original_filename: asset.original_file_name,
            file_size: 0,
            imported_at,
            media_type,
            taken_at,
            width: asset.width,
            height: asset.height,
            orientation: 1,
            duration_ms,
            is_favorite: asset.is_favorite,
            is_trashed,
            trashed_at,
        };

        let media_id = record.id.clone();
        self.library.media().upsert_media(&record).await?;

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
        self.events.send(AppEvent::AssetSynced { item });

        // Queue thumbnail download.
        self.library
            .thumbnails()
            .insert_thumbnail_pending(&media_id)
            .await?;
        if self.thumbnail_tx.send(media_id).await.is_err() {
            debug!("thumbnail channel closed, skipping download");
        }

        Ok(())
    }

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
            gps_alt: None,
            color_space: exif.profile_description,
        };

        self.library.metadata().upsert_metadata(&record).await
    }

    #[instrument(skip(self))]
    async fn handle_asset_delete(&self, asset_id: &str) -> Result<(), LibraryError> {
        let id = MediaId::new(asset_id.to_owned());
        self.library
            .delete_permanently_from_sync(std::slice::from_ref(&id))
            .await?;
        self.events
            .send(AppEvent::AssetDeletedRemote { media_id: id });
        Ok(())
    }

    #[instrument(skip(self, album), fields(album_id = %album.id, name = %album.name))]
    async fn handle_album(&self, album: SyncAlbumV1) -> Result<(), LibraryError> {
        let created_at = parse_datetime(&Some(album.created_at)).unwrap_or(0);
        let updated_at = parse_datetime(&Some(album.updated_at)).unwrap_or(0);

        self.library
            .albums()
            .upsert_album(
                &album.id,
                &album.name,
                created_at,
                updated_at,
                Some(&album.id),
            )
            .await?;

        self.events.send(AppEvent::AlbumCreated {
            id: AlbumId::from_raw(album.id),
            name: album.name,
        });

        Ok(())
    }

    #[instrument(skip(self))]
    async fn handle_album_delete(&self, album_id: &str) -> Result<(), LibraryError> {
        let id = AlbumId::from_raw(album_id.to_owned());
        self.library.albums().delete_album(&id).await?;
        self.events.send(AppEvent::AlbumDeleted { id });
        Ok(())
    }

    async fn handle_album_asset(&self, assoc: SyncAlbumToAssetV1) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().timestamp();
        self.db
            .upsert_album_media(&assoc.album_id, &assoc.asset_id, now)
            .await?;
        self.events.send(AppEvent::AlbumMediaChanged {
            album_id: AlbumId::from_raw(assoc.album_id),
        });
        Ok(())
    }

    async fn handle_album_asset_delete(
        &self,
        assoc: SyncAlbumToAssetDeleteV1,
    ) -> Result<(), LibraryError> {
        self.db
            .delete_album_media_entry(&assoc.album_id, &assoc.asset_id)
            .await?;
        self.events.send(AppEvent::AlbumMediaChanged {
            album_id: AlbumId::from_raw(assoc.album_id),
        });
        Ok(())
    }

    #[instrument(skip(self, person), fields(person_id = %person.id, name = %person.name))]
    async fn handle_person(&self, person: SyncPersonV1) -> Result<(), LibraryError> {
        self.library
            .faces()
            .upsert_person(
                &person.id,
                &person.name,
                person.birth_date.as_deref(),
                person.is_hidden,
                person.is_favorite,
                person.color.as_deref(),
                person.face_asset_id.as_deref(),
                Some(&person.id),
            )
            .await?;

        // Download person face thumbnail.
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
            source_type: face
                .source_type
                .unwrap_or_else(|| "MachineLearning".to_string()),
        };

        self.library.faces().upsert_asset_face(&row).await?;

        if let Some(ref person_id) = face.person_id {
            self.library.faces().update_face_count(person_id).await?;
        }

        Ok(())
    }

    // ── Sync infrastructure ─────────────────────────────────────────────

    async fn handle_sync_reset(
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
        self.library.faces().clear_asset_faces().await?;
        self.library.faces().clear_people().await?;
        self.db.clear_sync_checkpoints().await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_entity(
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
        let audit_id = self
            .db
            .start_sync_audit(entity_type, entity_id, sync_cycle)
            .await
            .ok();

        match handler_result.await {
            Ok(()) => {
                if let Some(aid) = audit_id {
                    let _ = self.db.complete_sync_audit(aid, audit_action).await;
                }
                acks.push(ack);
                *success_counter += 1;
            }
            Err(e) => {
                warn!(entity_type, entity_id, error = %e, "skipping sync entity");
                if let Some(aid) = audit_id {
                    let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                }
                *error_counter += 1;
            }
        }
    }

    async fn finish_sync(
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
                    self.library.delete_permanently_from_sync(&ids).await?;
                }
            }
        }

        if !acks.is_empty() {
            self.flush_acks(acks).await?;
        }

        self.events.send(AppEvent::SyncComplete {
            assets: counters.assets,
            people: counters.people,
            faces: counters.faces,
            errors: counters.errors,
        });

        if counters.people > 0 || counters.faces > 0 {
            self.events.send(AppEvent::PeopleSyncComplete);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, test_record};

    // ── SyncCounters ───────────────────────────────────────────────

    #[test]
    fn sync_counters_default_is_all_zero() {
        let c = SyncCounters::default();
        assert_eq!(c.assets, 0);
        assert_eq!(c.exifs, 0);
        assert_eq!(c.deletes, 0);
        assert_eq!(c.albums, 0);
        assert_eq!(c.people, 0);
        assert_eq!(c.faces, 0);
        assert_eq!(c.errors, 0);
    }

    // ── Database sync infrastructure ───────────────────────────────

    #[tokio::test]
    async fn sync_audit_start_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        let row_id = db
            .start_sync_audit("AssetV1", "uuid-1", "cycle-1")
            .await
            .unwrap();
        assert!(row_id > 0);

        db.complete_sync_audit(row_id, "upsert").await.unwrap();

        let row: (String, Option<String>) = sqlx::query_as(
            "SELECT action, completed_at FROM sync_audit WHERE id = ?",
        )
        .bind(row_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(row.0, "upsert");
        assert!(row.1.is_some()); // completed_at is set
    }

    #[tokio::test]
    async fn sync_audit_fail() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        let row_id = db
            .start_sync_audit("AssetV1", "uuid-fail", "cycle-2")
            .await
            .unwrap();

        db.fail_sync_audit(row_id, "parse error").await.unwrap();

        let row: (String, Option<String>) = sqlx::query_as(
            "SELECT action, error_msg FROM sync_audit WHERE id = ?",
        )
        .bind(row_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(row.0, "error");
        assert_eq!(row.1.as_deref(), Some("parse error"));
    }

    #[tokio::test]
    async fn sync_checkpoints_save_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        let pairs = vec![
            ("AssetV1".to_string(), "ack-asset-100".to_string()),
            ("AlbumV1".to_string(), "ack-album-50".to_string()),
        ];
        db.save_sync_checkpoints(&pairs).await.unwrap();

        let row: (String,) = sqlx::query_as(
            "SELECT ack FROM sync_checkpoints WHERE entity_type = 'AssetV1'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(row.0, "ack-asset-100");

        db.clear_sync_checkpoints().await.unwrap();

        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sync_checkpoints")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn sync_checkpoints_upsert_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        let pairs1 = vec![("AssetV1".to_string(), "ack-1".to_string())];
        db.save_sync_checkpoints(&pairs1).await.unwrap();

        let pairs2 = vec![("AssetV1".to_string(), "ack-2".to_string())];
        db.save_sync_checkpoints(&pairs2).await.unwrap();

        let row: (String,) = sqlx::query_as(
            "SELECT ack FROM sync_checkpoints WHERE entity_type = 'AssetV1'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(row.0, "ack-2");
    }

    #[tokio::test]
    async fn all_media_ids_returns_set() {
        use crate::library::db::test_helpers::record_with_taken_at;

        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        // Use different relative_paths to avoid UNIQUE constraint conflict.
        db.upsert_media(&record_with_taken_at(
            MediaId::new("id-a".to_string()),
            "2025/01/photo_a.jpg",
            Some(1_000),
        ))
        .await
        .unwrap();
        db.upsert_media(&record_with_taken_at(
            MediaId::new("id-b".to_string()),
            "2025/01/photo_b.jpg",
            Some(2_000),
        ))
        .await
        .unwrap();

        let ids = db.all_media_ids().await.unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("id-a"));
        assert!(ids.contains("id-b"));
    }

    #[tokio::test]
    async fn all_media_ids_empty_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        let ids = db.all_media_ids().await.unwrap();
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn upsert_album_media_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        // First insert an album and a media record (foreign keys).
        let now = chrono::Utc::now().timestamp();
        sqlx::query("INSERT INTO albums (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind("alb-1")
            .bind("Test")
            .bind(now)
            .bind(now)
            .execute(db.pool())
            .await
            .unwrap();
        db.upsert_media(&test_record(MediaId::new("med-1".to_string())))
            .await
            .unwrap();

        db.upsert_album_media("alb-1", "med-1", now).await.unwrap();

        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM album_media WHERE album_id = 'alb-1'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(count.0, 1);

        // Insert again should be ignored (INSERT OR IGNORE).
        db.upsert_album_media("alb-1", "med-1", now).await.unwrap();
        let count2: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM album_media WHERE album_id = 'alb-1'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(count2.0, 1);

        db.delete_album_media_entry("alb-1", "med-1")
            .await
            .unwrap();
        let count3: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM album_media WHERE album_id = 'alb-1'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(count3.0, 0);
    }

    #[tokio::test]
    async fn save_sync_checkpoints_empty_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;

        // Empty list should not error.
        db.save_sync_checkpoints(&[]).await.unwrap();
    }

    /// Test the checkpoint extraction logic from flush_acks.
    /// Since flush_acks calls the Immich API, we test the parsing logic directly.
    #[test]
    fn checkpoint_extraction_keeps_latest_per_entity_type() {
        let acks = vec![
            "AssetV1|ack-1".to_string(),
            "AssetV1|ack-2".to_string(),
            "AlbumV1|ack-3".to_string(),
            "AssetV1|ack-4".to_string(),
        ];

        let mut checkpoints: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for ack in &acks {
            if let Some(entity_type) = ack.split('|').next() {
                checkpoints.insert(entity_type.to_string(), ack.clone());
            }
        }

        assert_eq!(checkpoints.len(), 2);
        assert_eq!(checkpoints["AssetV1"], "AssetV1|ack-4");
        assert_eq!(checkpoints["AlbumV1"], "AlbumV1|ack-3");
    }
}

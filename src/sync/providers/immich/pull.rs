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
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::Library;

use super::client::ImmichClient;
use super::handlers::{self, CounterKind, SyncContext};
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

impl SyncCounters {
    fn increment(&mut self, kind: CounterKind) {
        match kind {
            CounterKind::Assets => self.assets += 1,
            CounterKind::Exifs => self.exifs += 1,
            CounterKind::Deletes => self.deletes += 1,
            CounterKind::Albums => self.albums += 1,
            CounterKind::People => self.people += 1,
            CounterKind::Faces => self.faces += 1,
            CounterKind::None => {}
        }
    }
}

/// Background pull sync engine for the Immich backend.
pub(crate) struct PullManager {
    pub client: ImmichClient,
    pub library: Arc<Library>,
    /// Database handle for sync infrastructure (checkpoints, audit).
    pub db: Database,
    pub events: EventSender,
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
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

        let entity_handlers = handlers::all_handlers();
        let ctx = SyncContext {
            client: self.client.clone(),
            library: Arc::clone(&self.library),
            db: self.db.clone(),
            events: self.events.clone(),
            thumbnails_dir: self.thumbnails_dir.clone(),
        };

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

            let entity_type = sync_line.entity_type.as_str();

            // ── Reset tracking ──────────────────────────────────────
            // SyncResetV1 sets the reset flag and loads existing IDs.
            // AssetV1/AssetDeleteV1 remove IDs from the tracking set.
            if entity_type == "SyncResetV1" {
                warn!("server requested sync reset — performing full resync");
                is_reset = true;
                let ids = self.db.all_media_ids().await?;
                info!(
                    existing_count = ids.len(),
                    "loaded existing media IDs for reset tracking"
                );
                existing_ids = Some(ids);
            }

            // ── Dispatch to handler ─────────────────────────────────
            if let Some(handler) = entity_handlers.iter().find(|h| h.entity_type() == entity_type)
            {
                let audit_id = self
                    .db
                    .start_sync_audit(entity_type, "", &sync_cycle)
                    .await
                    .ok();

                match handler.handle(&sync_line.data, line_number, &ctx).await {
                    Ok(result) => {
                        if let Some(aid) = audit_id {
                            let _ = self
                                .db
                                .complete_sync_audit(aid, result.audit_action)
                                .await;
                        }
                        acks.push(sync_line.ack);
                        counters.increment(result.counter);

                        // Track asset IDs for reset orphan detection.
                        if let Some(ref mut ids) = existing_ids {
                            if !result.entity_id.is_empty() {
                                ids.remove(&result.entity_id);
                            }
                        }
                    }
                    Err(e) => {
                        warn!(entity_type, error = %e, "skipping sync entity");
                        if let Some(aid) = audit_id {
                            let _ = self.db.fail_sync_audit(aid, &e.to_string()).await;
                        }
                        counters.errors += 1;
                    }
                }

                if counters.assets % 500 == 0 && counters.assets > 0 {
                    info!(assets = counters.assets, "sync progress");
                }
            } else if entity_type == "SyncCompleteV1" {
                // Not dispatched through handlers — breaks the loop.
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
            } else {
                debug!(
                    entity_type,
                    line_number, "ignoring unknown sync entity type"
                );
                acks.push(sync_line.ack);
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

    // ── Sync infrastructure ─────────────────────────────────────────────

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

            // Save checkpoints after each successful chunk so that a
            // failure in a later chunk doesn't lose already-acked progress.
            let mut checkpoints: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for ack in chunk {
                if let Some(entity_type) = ack.split('|').next() {
                    checkpoints.insert(entity_type.to_string(), ack.clone());
                }
            }
            let pairs: Vec<(String, String)> = checkpoints.into_iter().collect();
            self.db.save_sync_checkpoints(&pairs).await?;
        }

        acks.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, test_record};
    use crate::library::media::MediaId;

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

    #[test]
    fn sync_counters_increment() {
        let mut c = SyncCounters::default();
        c.increment(CounterKind::Assets);
        c.increment(CounterKind::Assets);
        c.increment(CounterKind::Deletes);
        c.increment(CounterKind::None);
        assert_eq!(c.assets, 2);
        assert_eq!(c.deletes, 1);
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
        assert!(row.1.is_some());
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
}

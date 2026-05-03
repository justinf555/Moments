//! Push sync manager — drains the outbox and pushes local mutations to Immich.
//!
//! Reads pending entries from the `sync_outbox` table, maps each to an
//! Immich API call, and marks entries as done or failed.

use tracing::{debug, error, info, instrument, warn};

use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::sync::event::SyncEvent;
use crate::sync::outbox::{OutboxMutation, OutboxStatus};

use super::client::ImmichClient;

/// A pending outbox entry read from the database.
#[derive(Debug)]
struct OutboxEntry {
    id: i64,
    entity_type: String,
    entity_id: String,
    action: String,
    payload: Option<String>,
    /// Number of times this entry has previously been attempted.
    attempts: i64,
}

/// How many entries to process per push cycle.
const BATCH_SIZE: i64 = 100;

/// After this many consecutive failures an entry is moved to
/// [`OutboxStatus::DeadLetter`] and stops being retried.
const MAX_ATTEMPTS: i64 = 10;

/// Cap stored error messages so a misbehaving server can't bloat the DB.
const MAX_ERROR_LEN: usize = 500;

/// Cap exponential backoff at one hour. Without a cap, attempt 10 would
/// sleep for ~17 hours.
const MAX_BACKOFF_SECS: i64 = 3600;

/// Compute when a row that just failed becomes eligible again.
///
/// `60 * 2^attempts`, capped at `MAX_BACKOFF_SECS`. attempts=1 → 120 s,
/// attempts=2 → 240 s, …, attempts=6 → 3840 s → capped to 3600.
fn backoff_seconds(attempts: i64) -> i64 {
    let exp = attempts.clamp(1, 30) as u32;
    let unclamped = 60_i64.saturating_mul(1_i64 << exp.min(20));
    unclamped.min(MAX_BACKOFF_SECS)
}

/// Truncate an error message to `MAX_ERROR_LEN` chars without splitting
/// a UTF-8 codepoint.
fn truncate_error(err: &str) -> String {
    if err.len() <= MAX_ERROR_LEN {
        return err.to_owned();
    }
    let mut end = MAX_ERROR_LEN;
    while !err.is_char_boundary(end) {
        end -= 1;
    }
    err[..end].to_owned()
}

/// Push sync engine — drains the outbox and makes Immich API calls.
pub(crate) struct PushManager {
    pub client: ImmichClient,
    pub db: Database,
    /// Channel for UI state updates (sync progress, errors).
    pub sync_events: tokio::sync::mpsc::UnboundedSender<SyncEvent>,
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
    pub interval_rx: tokio::sync::Mutex<tokio::sync::watch::Receiver<u64>>,
}

impl PushManager {
    /// Main push loop. Runs after each pull cycle interval.
    #[instrument(skip(self))]
    pub async fn run(&self) -> Result<(), LibraryError> {
        info!("push manager starting");

        loop {
            if *self.shutdown_rx.borrow() {
                info!("push manager shutting down");
                break;
            }

            if let Err(e) = self.push_pending().await {
                error!("push cycle failed: {e}");
                let sync_event = if crate::sync::event::is_connectivity_error(&e) {
                    SyncEvent::Offline
                } else {
                    SyncEvent::Error {
                        message: e.to_string(),
                    }
                };
                let _ = self.sync_events.send(sync_event);
            }

            // Purge completed entries periodically.
            if let Err(e) = self.purge_completed().await {
                warn!("failed to purge completed outbox entries: {e}");
            }

            let interval_secs: u64 = {
                let mut rx = self.interval_rx.lock().await;
                let val = *rx.borrow_and_update();
                val
            };
            if interval_secs == 0 {
                info!("push polling disabled (interval=0)");
                break;
            }

            let interval = std::time::Duration::from_secs(interval_secs);
            let mut shutdown = self.shutdown_rx.clone();
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = shutdown.changed() => {
                    info!("push manager shutting down during sleep");
                    break;
                }
            }
        }

        info!("push manager stopped");
        Ok(())
    }

    /// Process one batch of pending outbox entries.
    #[instrument(skip(self))]
    async fn push_pending(&self) -> Result<(), LibraryError> {
        let entries = self.fetch_pending().await?;
        if entries.is_empty() {
            debug!("no pending outbox entries");
            return Ok(());
        }

        let total = entries.len();
        info!(count = total, "pushing outbox entries");
        let _ = self
            .sync_events
            .send(SyncEvent::Processing { items: total });

        let mut pushed = 0usize;
        let mut errors = 0usize;
        // Track the worst stuck entry in this batch so we can surface
        // a single SyncEvent::Error for the sidebar status bar.
        let mut stuck: Option<(i64, String)> = None;

        for entry in &entries {
            match self.push_entry(entry).await {
                Ok(()) => {
                    self.mark_done(entry.id).await?;
                    pushed += 1;
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    warn!(
                        id = entry.id,
                        entity_type = %entry.entity_type,
                        action = %entry.action,
                        attempts = entry.attempts,
                        error = %err_msg,
                        "push failed"
                    );
                    let new_attempts = entry.attempts + 1;
                    self.mark_failed(entry.id, new_attempts, &err_msg).await?;
                    errors += 1;
                    if stuck.as_ref().is_none_or(|(a, _)| new_attempts > *a) {
                        stuck = Some((new_attempts, err_msg));
                    }
                }
            }
        }

        // If anything has been retried more than once, hint the user via
        // the sidebar so a poisoned entry doesn't fail silently forever.
        if let Some((attempts, msg)) = stuck.filter(|(a, _)| *a > 1) {
            let _ = self.sync_events.send(SyncEvent::Error {
                message: format!("Sync entry stuck (attempt {attempts}): {msg}"),
            });
        }

        let _ = self.sync_events.send(SyncEvent::Complete {
            items: pushed,
            errors,
        });

        Ok(())
    }

    /// Map a single outbox entry to an Immich API call.
    async fn push_entry(&self, entry: &OutboxEntry) -> Result<(), LibraryError> {
        let outbox_row = crate::library::mutation::OutboxRow {
            entity_type: entry.entity_type.clone(),
            entity_id: entry.entity_id.clone(),
            action: entry.action.clone(),
            payload: entry.payload.clone(),
        };
        let Some(mutation) = OutboxMutation::from_row(&outbox_row) else {
            warn!(
                entity_type = %entry.entity_type,
                action = %entry.action,
                "unknown outbox action, skipping"
            );
            return Ok(());
        };

        match mutation {
            // ── Asset mutations ──────────────────────────────────────
            OutboxMutation::AssetImported { .. } => self.push_asset_import(entry).await,

            OutboxMutation::AssetFavorited { id, favorite } => {
                let external_id = self.lookup_media_external_id(id.as_str()).await?;
                self.client
                    .put_no_content(
                        "/assets",
                        &serde_json::json!({
                            "ids": [external_id],
                            "isFavorite": favorite,
                        }),
                    )
                    .await
            }

            OutboxMutation::AssetTrashed { id } => {
                let external_id = self.lookup_media_external_id(id.as_str()).await?;
                self.client
                    .delete_with_body("/assets", &serde_json::json!({ "ids": [external_id] }))
                    .await
            }

            OutboxMutation::AssetRestored { id } => {
                let external_id = self.lookup_media_external_id(id.as_str()).await?;
                self.client
                    .post_no_content(
                        "/trash/restore/assets",
                        &serde_json::json!({ "ids": [external_id] }),
                    )
                    .await
            }

            OutboxMutation::AssetDeleted { id, external_id } => {
                let Some(external_id) = external_id else {
                    warn!(id = %id, "no external_id for deleted asset, skipping push");
                    return Ok(());
                };
                self.client
                    .delete_with_body(
                        "/assets",
                        &serde_json::json!({
                            "ids": [external_id],
                            "force": true,
                        }),
                    )
                    .await
            }

            // ── Album mutations ─────────────────────────────────────
            OutboxMutation::AlbumCreated { id, name } => {
                let resp: serde_json::Value = self
                    .client
                    .post("/albums", &serde_json::json!({ "albumName": name }))
                    .await?;
                if let Some(server_id) = resp["id"].as_str() {
                    self.set_album_external_id(id.as_str(), server_id).await?;
                }
                Ok(())
            }

            OutboxMutation::AlbumRenamed { id, name } => {
                let external_id = self.lookup_album_external_id(id.as_str()).await?;
                self.client
                    .patch_no_content(
                        &format!("/albums/{external_id}"),
                        &serde_json::json!({ "albumName": name }),
                    )
                    .await
            }

            OutboxMutation::AlbumDeleted { id, external_id } => {
                let Some(external_id) = external_id else {
                    warn!(id = %id, "no external_id for deleted album, skipping push");
                    return Ok(());
                };
                self.client
                    .delete_no_content(&format!("/albums/{external_id}"))
                    .await
            }

            OutboxMutation::AlbumMediaAdded {
                album_id,
                media_ids,
            } => {
                let external_id = self.lookup_album_external_id(album_id.as_str()).await?;
                let external_media_ids = self.resolve_media_external_ids_list(&media_ids).await?;
                self.client
                    .put_no_content(
                        &format!("/albums/{external_id}/assets"),
                        &serde_json::json!({ "ids": external_media_ids }),
                    )
                    .await
            }

            OutboxMutation::AlbumMediaRemoved {
                album_id,
                media_ids,
            } => {
                let external_id = self.lookup_album_external_id(album_id.as_str()).await?;
                let external_media_ids = self.resolve_media_external_ids_list(&media_ids).await?;
                self.client
                    .delete_with_body(
                        &format!("/albums/{external_id}/assets"),
                        &serde_json::json!({ "ids": external_media_ids }),
                    )
                    .await
            }

            // ── People mutations ────────────────────────────────────
            OutboxMutation::PersonRenamed { id, name } => {
                let external_id = self.lookup_person_external_id(id.as_str()).await?;
                self.client
                    .put_no_content(
                        &format!("/people/{external_id}"),
                        &serde_json::json!({ "name": name }),
                    )
                    .await
            }

            OutboxMutation::PersonHidden { id, hidden } => {
                let external_id = self.lookup_person_external_id(id.as_str()).await?;
                self.client
                    .put_no_content(
                        &format!("/people/{external_id}"),
                        &serde_json::json!({ "isHidden": hidden }),
                    )
                    .await
            }
        }
    }

    /// Upload a new asset to Immich from an outbox import entry.
    async fn push_asset_import(&self, entry: &OutboxEntry) -> Result<(), LibraryError> {
        let payload = self.parse_payload(entry)?;
        let file_path = payload["file_path"]
            .as_str()
            .ok_or_else(|| LibraryError::Immich("import entry missing file_path".to_string()))?;

        let path = std::path::Path::new(file_path);
        if !path.exists() {
            return Err(LibraryError::Immich(format!(
                "import file not found: {file_path}"
            )));
        }

        // Originals are stored at extensionless UUID-sharded paths, so
        // `path.file_name()` yields a bare UUID with no clue to the
        // file type. Immich infers media type from the multipart
        // filename's extension, so we must send the user-facing
        // `original_filename` (e.g. `IMG_1234.jpg`) instead.
        let filename = self.lookup_original_filename(&entry.entity_id).await?;

        let now = chrono::Utc::now().to_rfc3339();
        let resp = self
            .client
            .upload_asset(path, &filename, &entry.entity_id, &now, &now, None)
            .await?;

        // Store the server-assigned ID as external_id.
        if resp.id.is_empty() {
            return Err(LibraryError::Immich(format!(
                "upload returned empty id for media {}",
                entry.entity_id
            )));
        }
        self.set_media_external_id(&entry.entity_id, &resp.id)
            .await?;

        debug!(
            media_id = %entry.entity_id,
            server_id = %resp.id,
            status = %resp.status,
            "asset uploaded"
        );
        Ok(())
    }

    // ── Database helpers ─────────────────────────────────────────────

    async fn fetch_pending(&self) -> Result<Vec<OutboxEntry>, LibraryError> {
        let now = chrono::Utc::now().timestamp();
        let rows: Vec<(i64, String, String, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT id, entity_type, entity_id, action, payload, attempts
             FROM sync_outbox
             WHERE status IN (?, ?) AND next_attempt_at <= ?
             ORDER BY id ASC LIMIT ?",
        )
        .bind(OutboxStatus::Pending as i64)
        .bind(OutboxStatus::Failed as i64)
        .bind(now)
        .bind(BATCH_SIZE)
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(rows
            .into_iter()
            .map(
                |(id, entity_type, entity_id, action, payload, attempts)| OutboxEntry {
                    id,
                    entity_type,
                    entity_id,
                    action,
                    payload,
                    attempts,
                },
            )
            .collect())
    }

    async fn mark_done(&self, id: i64) -> Result<(), LibraryError> {
        sqlx::query("UPDATE sync_outbox SET status = ? WHERE id = ?")
            .bind(OutboxStatus::Done as i64)
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Record a push failure. After [`MAX_ATTEMPTS`] the entry is moved
    /// to [`OutboxStatus::DeadLetter`] and stops being retried.
    async fn mark_failed(
        &self,
        id: i64,
        new_attempts: i64,
        error: &str,
    ) -> Result<(), LibraryError> {
        let truncated = truncate_error(error);
        if new_attempts >= MAX_ATTEMPTS {
            sqlx::query(
                "UPDATE sync_outbox
                 SET status = ?, attempts = ?, last_error = ?, next_attempt_at = 0
                 WHERE id = ?",
            )
            .bind(OutboxStatus::DeadLetter as i64)
            .bind(new_attempts)
            .bind(&truncated)
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
            warn!(
                id,
                attempts = new_attempts,
                "outbox entry exceeded MAX_ATTEMPTS, moved to dead-letter"
            );
        } else {
            let now = chrono::Utc::now().timestamp();
            let next_attempt_at = now.saturating_add(backoff_seconds(new_attempts));
            sqlx::query(
                "UPDATE sync_outbox
                 SET status = ?, attempts = ?, last_error = ?, next_attempt_at = ?
                 WHERE id = ?",
            )
            .bind(OutboxStatus::Failed as i64)
            .bind(new_attempts)
            .bind(&truncated)
            .bind(next_attempt_at)
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        }
        Ok(())
    }

    async fn purge_completed(&self) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM sync_outbox WHERE status = ?")
            .bind(OutboxStatus::Done as i64)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    // ── Media row lookups ───────────────────────────────────────────

    /// Fetch the user-facing original filename for an asset.
    ///
    /// Required for upload — Immich keys media-type detection off the
    /// multipart filename's extension, and the extensionless UUID-sharded
    /// path on disk doesn't carry one.
    async fn lookup_original_filename(&self, local_id: &str) -> Result<String, LibraryError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT original_filename FROM media WHERE id = ?")
                .bind(local_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        match row {
            Some((name,)) if !name.is_empty() => Ok(name),
            Some(_) => Err(LibraryError::Immich(format!(
                "media has empty original_filename: {local_id}"
            ))),
            None => Err(LibraryError::Immich(format!("media not found: {local_id}"))),
        }
    }

    async fn lookup_media_external_id(&self, local_id: &str) -> Result<String, LibraryError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT COALESCE(external_id, id) FROM media WHERE id = ?")
                .bind(local_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        match row {
            Some((eid,)) => Ok(eid),
            None => Err(LibraryError::Immich(format!("media not found: {local_id}"))),
        }
    }

    async fn lookup_album_external_id(&self, local_id: &str) -> Result<String, LibraryError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT COALESCE(external_id, id) FROM albums WHERE id = ?")
                .bind(local_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        match row {
            Some((eid,)) => Ok(eid),
            None => Err(LibraryError::Immich(format!("album not found: {local_id}"))),
        }
    }

    async fn lookup_person_external_id(&self, local_id: &str) -> Result<String, LibraryError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT COALESCE(external_id, id) FROM people WHERE id = ?")
                .bind(local_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        match row {
            Some((eid,)) => Ok(eid),
            None => Err(LibraryError::Immich(format!(
                "person not found: {local_id}"
            ))),
        }
    }

    async fn set_media_external_id(
        &self,
        local_id: &str,
        external_id: &str,
    ) -> Result<(), LibraryError> {
        sqlx::query("UPDATE media SET external_id = ? WHERE id = ?")
            .bind(external_id)
            .bind(local_id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    async fn set_album_external_id(
        &self,
        local_id: &str,
        external_id: &str,
    ) -> Result<(), LibraryError> {
        sqlx::query("UPDATE albums SET external_id = ? WHERE id = ?")
            .bind(external_id)
            .bind(local_id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    // ── Payload helpers ─────────────────────────────────────────────

    fn parse_payload(&self, entry: &OutboxEntry) -> Result<serde_json::Value, LibraryError> {
        let raw = entry.payload.as_deref().unwrap_or("{}");
        serde_json::from_str(raw).map_err(|e| {
            LibraryError::Immich(format!(
                "invalid outbox payload for {} {}: {e}",
                entry.entity_type, entry.action
            ))
        })
    }

    /// Resolve local media IDs in a JSON payload to their Immich external IDs.
    #[cfg(test)]
    async fn resolve_media_external_ids(
        &self,
        payload: &serde_json::Value,
    ) -> Result<Vec<String>, LibraryError> {
        let local_ids: Vec<&str> = payload["media_ids"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        let mut external_ids = Vec::with_capacity(local_ids.len());
        for local_id in local_ids {
            external_ids.push(self.lookup_media_external_id(local_id).await?);
        }
        Ok(external_ids)
    }

    /// Resolve typed media IDs to their Immich external IDs.
    async fn resolve_media_external_ids_list(
        &self,
        ids: &[crate::library::media::MediaId],
    ) -> Result<Vec<String>, LibraryError> {
        let mut external_ids = Vec::with_capacity(ids.len());
        for id in ids {
            external_ids.push(self.lookup_media_external_id(id.as_str()).await?);
        }
        Ok(external_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, test_record};
    use crate::library::media::MediaId;

    /// Helper: create DB, insert outbox entries, return a PushManager.
    /// We cannot call API methods (no real server), but we can test DB helpers.
    async fn setup_push_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        (dir, db)
    }

    async fn insert_outbox_entry(
        db: &Database,
        entity_type: &str,
        entity_id: &str,
        action: &str,
        payload: Option<&str>,
    ) {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO sync_outbox (entity_type, entity_id, action, payload, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(action)
        .bind(payload)
        .bind(now)
        .execute(db.pool())
        .await
        .unwrap();
    }

    /// Create a PushManager with a real DB for testing DB helpers.
    async fn make_push_manager(db: Database) -> PushManager {
        let client = ImmichClient::new("https://test.example.com", "fake-token").unwrap();

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_interval_tx, interval_rx) = tokio::sync::watch::channel(60u64);
        let (sync_events, _rx) = tokio::sync::mpsc::unbounded_channel();

        PushManager {
            client,
            db,
            sync_events,
            shutdown_rx,
            interval_rx: tokio::sync::Mutex::new(interval_rx),
        }
    }

    #[tokio::test]
    async fn fetch_pending_returns_ordered_entries() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "a1", "trash", None).await;
        insert_outbox_entry(&db, "asset", "a2", "favorite", None).await;
        insert_outbox_entry(&db, "album", "b1", "create", Some(r#"{"name":"Test"}"#)).await;

        let push = make_push_manager(db).await;
        let entries = push.fetch_pending().await.unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].entity_id, "a1");
        assert_eq!(entries[1].entity_id, "a2");
        assert_eq!(entries[2].entity_id, "b1");
        assert_eq!(entries[2].payload.as_deref(), Some(r#"{"name":"Test"}"#));
    }

    #[tokio::test]
    async fn fetch_pending_empty_returns_empty() {
        let (_dir, db) = setup_push_db().await;
        let push = make_push_manager(db).await;
        let entries = push.fetch_pending().await.unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn fetch_pending_returns_pending_and_failed_skips_done() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "pending", "trash", None).await;
        insert_outbox_entry(&db, "asset", "done", "trash", None).await;
        insert_outbox_entry(&db, "asset", "failed", "trash", None).await;

        // Mark second as done (status=1), third as failed (status=2).
        sqlx::query("UPDATE sync_outbox SET status = 1 WHERE entity_id = 'done'")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE sync_outbox SET status = 2 WHERE entity_id = 'failed'")
            .execute(db.pool())
            .await
            .unwrap();

        let push = make_push_manager(db).await;
        let entries = push.fetch_pending().await.unwrap();
        // Both pending (status=0) and failed (status=2) are retried.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entity_id, "pending");
        assert_eq!(entries[1].entity_id, "failed");
    }

    #[tokio::test]
    async fn mark_done_sets_status_to_one() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "a1", "trash", None).await;

        let push = make_push_manager(db.clone()).await;
        push.mark_done(1).await.unwrap();

        let row: (i64,) = sqlx::query_as("SELECT status FROM sync_outbox WHERE id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(row.0, 1);
    }

    #[tokio::test]
    async fn mark_failed_first_attempt_records_error_and_schedules_backoff() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "a1", "trash", Some("original")).await;

        let push = make_push_manager(db.clone()).await;
        let before = chrono::Utc::now().timestamp();
        push.mark_failed(1, 1, "connection timeout").await.unwrap();

        let row: (i64, i64, Option<String>, i64, Option<String>) = sqlx::query_as(
            "SELECT status, attempts, last_error, next_attempt_at, payload
             FROM sync_outbox WHERE id = 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();

        assert_eq!(row.0, OutboxStatus::Failed as i64);
        assert_eq!(row.1, 1);
        assert_eq!(row.2.as_deref(), Some("connection timeout"));
        // backoff_seconds(1) = 120
        assert!(row.3 >= before + 120);
        // Payload preserved for retry.
        assert_eq!(row.4.as_deref(), Some("original"));
    }

    #[tokio::test]
    async fn mark_failed_at_max_attempts_moves_to_dead_letter() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "a1", "trash", None).await;

        let push = make_push_manager(db.clone()).await;
        push.mark_failed(1, MAX_ATTEMPTS, "permanent server error")
            .await
            .unwrap();

        let row: (i64, i64, Option<String>) =
            sqlx::query_as("SELECT status, attempts, last_error FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();

        assert_eq!(row.0, OutboxStatus::DeadLetter as i64);
        assert_eq!(row.1, MAX_ATTEMPTS);
        assert_eq!(row.2.as_deref(), Some("permanent server error"));
    }

    #[tokio::test]
    async fn mark_failed_truncates_long_error() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "a1", "trash", None).await;

        let push = make_push_manager(db.clone()).await;
        let huge = "x".repeat(MAX_ERROR_LEN * 4);
        push.mark_failed(1, 1, &huge).await.unwrap();

        let row: (Option<String>,) =
            sqlx::query_as("SELECT last_error FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        let stored = row.0.unwrap();
        assert_eq!(stored.len(), MAX_ERROR_LEN);
    }

    #[tokio::test]
    async fn fetch_pending_excludes_rows_in_backoff() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "ready", "trash", None).await;
        insert_outbox_entry(&db, "asset", "waiting", "trash", None).await;

        // Push 'waiting' an hour into the future via mark_failed.
        let push = make_push_manager(db.clone()).await;
        sqlx::query("UPDATE sync_outbox SET status = 2 WHERE entity_id = 'waiting'")
            .execute(db.pool())
            .await
            .unwrap();
        let future = chrono::Utc::now().timestamp() + 3600;
        sqlx::query("UPDATE sync_outbox SET next_attempt_at = ? WHERE entity_id = 'waiting'")
            .bind(future)
            .execute(db.pool())
            .await
            .unwrap();

        let entries = push.fetch_pending().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entity_id, "ready");
    }

    #[tokio::test]
    async fn fetch_pending_excludes_dead_letters() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "alive", "trash", None).await;
        insert_outbox_entry(&db, "asset", "dead", "trash", None).await;

        sqlx::query("UPDATE sync_outbox SET status = ? WHERE entity_id = 'dead'")
            .bind(OutboxStatus::DeadLetter as i64)
            .execute(db.pool())
            .await
            .unwrap();

        let push = make_push_manager(db).await;
        let entries = push.fetch_pending().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entity_id, "alive");
    }

    #[test]
    fn backoff_grows_then_caps_at_one_hour() {
        // Linear-ish growth at first, then clamped.
        assert_eq!(backoff_seconds(1), 120);
        assert_eq!(backoff_seconds(2), 240);
        assert_eq!(backoff_seconds(3), 480);
        assert_eq!(backoff_seconds(5), 1920);
        assert_eq!(backoff_seconds(6), MAX_BACKOFF_SECS);
        assert_eq!(backoff_seconds(20), MAX_BACKOFF_SECS);
    }

    #[test]
    fn truncate_error_respects_char_boundaries() {
        let s = "é".repeat(MAX_ERROR_LEN); // each char is 2 bytes
        let out = truncate_error(&s);
        assert!(out.len() <= MAX_ERROR_LEN);
        // Must be valid UTF-8 (this would panic if we split a codepoint).
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[tokio::test]
    async fn purge_completed_keeps_failed_entries() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "a1", "trash", None).await;
        insert_outbox_entry(&db, "asset", "a2", "trash", None).await;
        insert_outbox_entry(&db, "asset", "a3", "trash", None).await;

        // a1 = done, a2 = failed, a3 = pending.
        sqlx::query("UPDATE sync_outbox SET status = 1 WHERE entity_id = 'a1'")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE sync_outbox SET status = 2 WHERE entity_id = 'a2'")
            .execute(db.pool())
            .await
            .unwrap();

        let push = make_push_manager(db.clone()).await;
        push.purge_completed().await.unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sync_outbox")
            .fetch_one(db.pool())
            .await
            .unwrap();
        // a2 (failed) and a3 (pending) remain — only a1 (done) was purged.
        assert_eq!(count.0, 2);
    }

    #[tokio::test]
    async fn parse_payload_valid_json() {
        let (_dir, db) = setup_push_db().await;
        let push = make_push_manager(db).await;

        let entry = OutboxEntry {
            id: 1,
            entity_type: "album".to_string(),
            entity_id: "alb1".to_string(),
            action: "create".to_string(),
            payload: Some(r#"{"name":"Photos"}"#.to_string()),
            attempts: 0,
        };

        let val = push.parse_payload(&entry).unwrap();
        assert_eq!(val["name"], "Photos");
    }

    #[tokio::test]
    async fn parse_payload_none_returns_empty_object() {
        let (_dir, db) = setup_push_db().await;
        let push = make_push_manager(db).await;

        let entry = OutboxEntry {
            id: 1,
            entity_type: "asset".to_string(),
            entity_id: "a1".to_string(),
            action: "trash".to_string(),
            payload: None,
            attempts: 0,
        };

        let val = push.parse_payload(&entry).unwrap();
        assert!(val.is_object());
        assert_eq!(val.as_object().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn parse_payload_invalid_json_returns_error() {
        let (_dir, db) = setup_push_db().await;
        let push = make_push_manager(db).await;

        let entry = OutboxEntry {
            id: 1,
            entity_type: "album".to_string(),
            entity_id: "alb1".to_string(),
            action: "create".to_string(),
            payload: Some("not json".to_string()),
            attempts: 0,
        };

        let result = push.parse_payload(&entry);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn lookup_media_external_id_found() {
        let (_dir, db) = setup_push_db().await;

        // Insert a media record with external_id.
        let mut record = test_record(MediaId::new("local-1".to_string()));
        record.external_id = Some("immich-uuid-1".to_string());
        db.upsert_media(&record).await.unwrap();

        let push = make_push_manager(db).await;
        let ext_id = push.lookup_media_external_id("local-1").await.unwrap();
        assert_eq!(ext_id, "immich-uuid-1");
    }

    #[tokio::test]
    async fn lookup_media_external_id_missing_returns_error() {
        let (_dir, db) = setup_push_db().await;
        let push = make_push_manager(db).await;

        let result = push.lookup_media_external_id("nonexistent").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("media not found"));
    }

    #[tokio::test]
    async fn lookup_original_filename_returns_stored_name() {
        let (_dir, db) = setup_push_db().await;

        let mut record = test_record(MediaId::new("local-1".to_string()));
        record.original_filename = "IMG_1234.jpg".to_string();
        db.upsert_media(&record).await.unwrap();

        let push = make_push_manager(db).await;
        let name = push.lookup_original_filename("local-1").await.unwrap();
        assert_eq!(name, "IMG_1234.jpg");
    }

    #[tokio::test]
    async fn lookup_original_filename_missing_returns_error() {
        let (_dir, db) = setup_push_db().await;
        let push = make_push_manager(db).await;

        let result = push.lookup_original_filename("nope").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("media not found"));
    }

    #[tokio::test]
    async fn lookup_original_filename_empty_returns_error() {
        let (_dir, db) = setup_push_db().await;

        let mut record = test_record(MediaId::new("local-empty".to_string()));
        record.original_filename = String::new();
        db.upsert_media(&record).await.unwrap();

        let push = make_push_manager(db).await;
        let result = push.lookup_original_filename("local-empty").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("empty original_filename"));
    }

    #[tokio::test]
    async fn lookup_media_external_id_null_falls_back_to_id() {
        let (_dir, db) = setup_push_db().await;

        // Insert a media record with no external_id — COALESCE returns id.
        let record = test_record(MediaId::new("local-no-ext".to_string()));
        db.upsert_media(&record).await.unwrap();

        let push = make_push_manager(db).await;
        let result = push.lookup_media_external_id("local-no-ext").await;
        assert_eq!(result.unwrap(), "local-no-ext");
    }

    #[tokio::test]
    async fn lookup_album_external_id_found() {
        let (_dir, db) = setup_push_db().await;

        // Insert an album with external_id.
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO albums (id, name, created_at, updated_at, external_id) VALUES (?, ?, ?, ?, ?)",
        )
        .bind("local-album")
        .bind("Test Album")
        .bind(now)
        .bind(now)
        .bind("immich-album-uuid")
        .execute(db.pool())
        .await
        .unwrap();

        let push = make_push_manager(db).await;
        let ext_id = push.lookup_album_external_id("local-album").await.unwrap();
        assert_eq!(ext_id, "immich-album-uuid");
    }

    #[tokio::test]
    async fn lookup_album_external_id_missing_returns_error() {
        let (_dir, db) = setup_push_db().await;
        let push = make_push_manager(db).await;

        let result = push.lookup_album_external_id("no-album").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn lookup_person_external_id_found() {
        let (_dir, db) = setup_push_db().await;

        sqlx::query(
            "INSERT INTO people (id, name, face_count, is_hidden, external_id) VALUES (?, ?, ?, ?, ?)",
        )
        .bind("local-person")
        .bind("Alice")
        .bind(5)
        .bind(false)
        .bind("immich-person-uuid")
        .execute(db.pool())
        .await
        .unwrap();

        let push = make_push_manager(db).await;
        let ext_id = push
            .lookup_person_external_id("local-person")
            .await
            .unwrap();
        assert_eq!(ext_id, "immich-person-uuid");
    }

    #[tokio::test]
    async fn lookup_person_external_id_missing_returns_error() {
        let (_dir, db) = setup_push_db().await;
        let push = make_push_manager(db).await;

        let result = push.lookup_person_external_id("no-person").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn set_media_external_id_updates_record() {
        let (_dir, db) = setup_push_db().await;

        let record = test_record(MediaId::new("local-m".to_string()));
        db.upsert_media(&record).await.unwrap();

        let push = make_push_manager(db.clone()).await;
        push.set_media_external_id("local-m", "new-ext-id")
            .await
            .unwrap();

        let row: (Option<String>,) =
            sqlx::query_as("SELECT external_id FROM media WHERE id = 'local-m'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0.as_deref(), Some("new-ext-id"));
    }

    #[tokio::test]
    async fn set_album_external_id_updates_record() {
        let (_dir, db) = setup_push_db().await;

        let now = chrono::Utc::now().timestamp();
        sqlx::query("INSERT INTO albums (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind("alb-local")
            .bind("My Album")
            .bind(now)
            .bind(now)
            .execute(db.pool())
            .await
            .unwrap();

        let push = make_push_manager(db.clone()).await;
        push.set_album_external_id("alb-local", "alb-ext-id")
            .await
            .unwrap();

        let row: (Option<String>,) =
            sqlx::query_as("SELECT external_id FROM albums WHERE id = 'alb-local'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0.as_deref(), Some("alb-ext-id"));
    }

    #[tokio::test]
    async fn resolve_media_external_ids_resolves_all() {
        use crate::library::db::test_helpers::record_with_taken_at;

        let (_dir, db) = setup_push_db().await;

        // Use different relative_paths to avoid UNIQUE constraint conflict.
        let mut r1 =
            record_with_taken_at(MediaId::new("m1".to_string()), "photos/a.jpg", Some(1_000));
        r1.external_id = Some("ext-m1".to_string());
        let mut r2 =
            record_with_taken_at(MediaId::new("m2".to_string()), "photos/b.jpg", Some(2_000));
        r2.external_id = Some("ext-m2".to_string());
        db.upsert_media(&r1).await.unwrap();
        db.upsert_media(&r2).await.unwrap();

        let push = make_push_manager(db).await;
        let payload = serde_json::json!({ "media_ids": ["m1", "m2"] });
        let ext_ids = push.resolve_media_external_ids(&payload).await.unwrap();
        assert_eq!(ext_ids, vec!["ext-m1", "ext-m2"]);
    }

    #[tokio::test]
    async fn resolve_media_external_ids_empty_payload() {
        let (_dir, db) = setup_push_db().await;
        let push = make_push_manager(db).await;

        let payload = serde_json::json!({});
        let ext_ids = push.resolve_media_external_ids(&payload).await.unwrap();
        assert!(ext_ids.is_empty());
    }
}

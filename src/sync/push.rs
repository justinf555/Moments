//! Push sync manager — drains the outbox and pushes local mutations to Immich.
//!
//! Reads pending entries from the `sync_outbox` table, maps each to an
//! Immich API call, and marks entries as done or failed.

use tracing::{debug, error, info, instrument, warn};

use crate::library::db::Database;
use crate::library::error::LibraryError;

use super::client::ImmichClient;

/// A pending outbox entry read from the database.
#[derive(Debug)]
struct OutboxEntry {
    id: i64,
    entity_type: String,
    entity_id: String,
    action: String,
    payload: Option<String>,
}

/// How many entries to process per push cycle.
const BATCH_SIZE: i64 = 100;

/// Push sync engine — drains the outbox and makes Immich API calls.
pub(crate) struct PushManager {
    pub client: ImmichClient,
    pub db: Database,
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

        info!(count = entries.len(), "pushing outbox entries");

        for entry in &entries {
            match self.push_entry(entry).await {
                Ok(()) => {
                    self.mark_done(entry.id).await?;
                }
                Err(e) => {
                    warn!(
                        id = entry.id,
                        entity_type = %entry.entity_type,
                        action = %entry.action,
                        error = %e,
                        "push failed, marking as failed"
                    );
                    self.mark_failed(entry.id, &e.to_string()).await?;
                }
            }
        }

        Ok(())
    }

    /// Map a single outbox entry to an Immich API call.
    async fn push_entry(&self, entry: &OutboxEntry) -> Result<(), LibraryError> {
        match (entry.entity_type.as_str(), entry.action.as_str()) {
            // ── Asset mutations ──────────────────────────────────────
            ("asset", "import") => {
                self.push_asset_import(entry).await
            }
            ("asset", "favorite") | ("asset", "unfavorite") => {
                let external_id = self.lookup_media_external_id(&entry.entity_id).await?;
                let is_favorite = entry.action == "favorite";
                self.client
                    .put_no_content(
                        "/assets",
                        &serde_json::json!({
                            "ids": [external_id],
                            "isFavorite": is_favorite,
                        }),
                    )
                    .await
            }
            ("asset", "trash") => {
                let external_id = self.lookup_media_external_id(&entry.entity_id).await?;
                self.client
                    .delete_with_body(
                        "/assets",
                        &serde_json::json!({ "ids": [external_id] }),
                    )
                    .await
            }
            ("asset", "restore") => {
                let external_id = self.lookup_media_external_id(&entry.entity_id).await?;
                self.client
                    .post_no_content(
                        "/trash/restore/assets",
                        &serde_json::json!({ "ids": [external_id] }),
                    )
                    .await
            }
            ("asset", "delete") => {
                let external_id = match self.lookup_media_external_id(&entry.entity_id).await {
                    Ok(eid) => eid,
                    Err(_) => {
                        debug!(id = %entry.entity_id, "asset already deleted, skipping push");
                        return Ok(());
                    }
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
            ("album", "create") => {
                let payload = self.parse_payload(entry)?;
                let name = payload["name"].as_str().unwrap_or("");
                let resp: serde_json::Value = self
                    .client
                    .post("/albums", &serde_json::json!({ "albumName": name }))
                    .await?;
                // Store the server-assigned album ID as external_id.
                if let Some(server_id) = resp["id"].as_str() {
                    self.set_album_external_id(&entry.entity_id, server_id)
                        .await?;
                }
                Ok(())
            }
            ("album", "rename") => {
                let external_id = self.lookup_album_external_id(&entry.entity_id).await?;
                let payload = self.parse_payload(entry)?;
                let name = payload["name"].as_str().unwrap_or("");
                self.client
                    .patch_no_content(
                        &format!("/albums/{external_id}"),
                        &serde_json::json!({ "albumName": name }),
                    )
                    .await
            }
            ("album", "delete") => {
                let external_id = match self.lookup_album_external_id(&entry.entity_id).await {
                    Ok(eid) => eid,
                    Err(_) => {
                        debug!(id = %entry.entity_id, "album already deleted, skipping push");
                        return Ok(());
                    }
                };
                self.client
                    .delete_no_content(&format!("/albums/{external_id}"))
                    .await
            }
            ("album", "add_media") => {
                let external_id = self.lookup_album_external_id(&entry.entity_id).await?;
                let payload = self.parse_payload(entry)?;
                let media_ids = self.resolve_media_external_ids(&payload).await?;
                self.client
                    .put_no_content(
                        &format!("/albums/{external_id}/assets"),
                        &serde_json::json!({ "ids": media_ids }),
                    )
                    .await
            }
            ("album", "remove_media") => {
                let external_id = self.lookup_album_external_id(&entry.entity_id).await?;
                let payload = self.parse_payload(entry)?;
                let media_ids = self.resolve_media_external_ids(&payload).await?;
                self.client
                    .delete_with_body(
                        &format!("/albums/{external_id}/assets"),
                        &serde_json::json!({ "ids": media_ids }),
                    )
                    .await
            }

            // ── People mutations ────────────────────────────────────
            ("person", "rename") => {
                let external_id = self.lookup_person_external_id(&entry.entity_id).await?;
                let payload = self.parse_payload(entry)?;
                let name = payload["name"].as_str().unwrap_or("");
                self.client
                    .put_no_content(
                        &format!("/people/{external_id}"),
                        &serde_json::json!({ "name": name }),
                    )
                    .await
            }
            ("person", "hide") => {
                let external_id = self.lookup_person_external_id(&entry.entity_id).await?;
                let payload = self.parse_payload(entry)?;
                let hidden = payload["hidden"].as_bool().unwrap_or(false);
                self.client
                    .put_no_content(
                        &format!("/people/{external_id}"),
                        &serde_json::json!({ "isHidden": hidden }),
                    )
                    .await
            }

            _ => {
                warn!(
                    entity_type = %entry.entity_type,
                    action = %entry.action,
                    "unknown outbox action, skipping"
                );
                Ok(())
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

        let now = chrono::Utc::now().to_rfc3339();
        let resp = self
            .client
            .upload_asset(path, &entry.entity_id, &now, &now, None)
            .await?;

        // Store the server-assigned ID as external_id.
        if !resp.id.is_empty() {
            self.set_media_external_id(&entry.entity_id, &resp.id)
                .await?;
        }

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
        let rows: Vec<(i64, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT id, entity_type, entity_id, action, payload
             FROM sync_outbox WHERE status = 0
             ORDER BY id ASC LIMIT ?",
        )
        .bind(BATCH_SIZE)
        .fetch_all(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(rows
            .into_iter()
            .map(|(id, entity_type, entity_id, action, payload)| OutboxEntry {
                id,
                entity_type,
                entity_id,
                action,
                payload,
            })
            .collect())
    }

    async fn mark_done(&self, id: i64) -> Result<(), LibraryError> {
        sqlx::query("UPDATE sync_outbox SET status = 1 WHERE id = ?")
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    async fn mark_failed(&self, id: i64, error: &str) -> Result<(), LibraryError> {
        sqlx::query("UPDATE sync_outbox SET status = 2, payload = ? WHERE id = ?")
            .bind(error)
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    async fn purge_completed(&self) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM sync_outbox WHERE status IN (1, 2)")
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    // ── External ID lookups ─────────────────────────────────────────

    async fn lookup_media_external_id(&self, local_id: &str) -> Result<String, LibraryError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT COALESCE(external_id, id) FROM media WHERE id = ?")
                .bind(local_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        match row {
            Some((eid,)) => Ok(eid),
            None => Err(LibraryError::Immich(format!(
                "media not found: {local_id}"
            ))),
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
            None => Err(LibraryError::Immich(format!(
                "album not found: {local_id}"
            ))),
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

    /// Resolve local media IDs in a payload to their Immich external IDs.
    async fn resolve_media_external_ids(
        &self,
        payload: &serde_json::Value,
    ) -> Result<Vec<String>, LibraryError> {
        let local_ids: Vec<&str> = payload["media_ids"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut external_ids = Vec::with_capacity(local_ids.len());
        for local_id in local_ids {
            external_ids.push(self.lookup_media_external_id(local_id).await?);
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

        PushManager {
            client,
            db,
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
    async fn fetch_pending_skips_done_and_failed() {
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
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entity_id, "pending");
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
    async fn mark_failed_sets_status_and_error() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "a1", "trash", None).await;

        let push = make_push_manager(db.clone()).await;
        push.mark_failed(1, "connection timeout").await.unwrap();

        let row: (i64, Option<String>) =
            sqlx::query_as("SELECT status, payload FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, 2);
        assert_eq!(row.1.as_deref(), Some("connection timeout"));
    }

    #[tokio::test]
    async fn purge_completed_removes_done_entries() {
        let (_dir, db) = setup_push_db().await;
        insert_outbox_entry(&db, "asset", "a1", "trash", None).await;
        insert_outbox_entry(&db, "asset", "a2", "trash", None).await;
        insert_outbox_entry(&db, "asset", "a3", "trash", None).await;

        // Mark a1 and a2 as done.
        sqlx::query("UPDATE sync_outbox SET status = 1 WHERE entity_id IN ('a1', 'a2')")
            .execute(db.pool())
            .await
            .unwrap();

        let push = make_push_manager(db.clone()).await;
        push.purge_completed().await.unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sync_outbox")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 1); // only a3 remains
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
        let mut r1 = record_with_taken_at(MediaId::new("m1".to_string()), "photos/a.jpg", Some(1_000));
        r1.external_id = Some("ext-m1".to_string());
        let mut r2 = record_with_taken_at(MediaId::new("m2".to_string()), "photos/b.jpg", Some(2_000));
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

//! Push sync manager — drains the outbox and pushes local mutations to Immich.
//!
//! Reads pending entries from the `sync_outbox` table, maps each to an
//! Immich API call, and marks entries as done or failed.

use std::sync::Arc;

use tracing::{debug, error, info, instrument, warn};

use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::Library;

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
    pub library: Arc<Library>,
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
                let external_id = self.lookup_media_external_id(&entry.entity_id).await?;
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
                let external_id = self.lookup_album_external_id(&entry.entity_id).await?;
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
        sqlx::query("DELETE FROM sync_outbox WHERE status = 1")
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    // ── External ID lookups ─────────────────────────────────────────

    async fn lookup_media_external_id(&self, local_id: &str) -> Result<String, LibraryError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT external_id FROM media WHERE id = ?")
                .bind(local_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        match row.and_then(|(eid,)| eid) {
            Some(eid) => Ok(eid),
            None => Err(LibraryError::Immich(format!(
                "no external_id for media {local_id}"
            ))),
        }
    }

    async fn lookup_album_external_id(&self, local_id: &str) -> Result<String, LibraryError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT external_id FROM albums WHERE id = ?")
                .bind(local_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        match row.and_then(|(eid,)| eid) {
            Some(eid) => Ok(eid),
            None => Err(LibraryError::Immich(format!(
                "no external_id for album {local_id}"
            ))),
        }
    }

    async fn lookup_person_external_id(&self, local_id: &str) -> Result<String, LibraryError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT external_id FROM people WHERE id = ?")
                .bind(local_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        match row.and_then(|(eid,)| eid) {
            Some(eid) => Ok(eid),
            None => Err(LibraryError::Immich(format!(
                "no external_id for person {local_id}"
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

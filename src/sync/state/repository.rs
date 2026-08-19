// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! `sync_checkpoints` + `sync_audit` table persistence.
//!
//! Both tables are owned by the Immich pull engine: checkpoints record
//! the last server ack per entity type so a restart resumes mid-stream,
//! and the audit log captures the start/finish of every entity record
//! processed so post-mortems can attribute errors.

use crate::library::db::Database;
use crate::library::error::LibraryError;

/// Repository for sync-engine state tables.
#[derive(Clone)]
pub struct SyncStateRepository {
    db: Database,
}

impl SyncStateRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    // ── Checkpoints ─────────────────────────────────────────────────

    /// Batch-upsert per-entity-type ack checkpoints. No-op on empty input.
    pub async fn save_checkpoints(&self, acks: &[(String, String)]) -> Result<(), LibraryError> {
        if acks.is_empty() {
            return Ok(());
        }
        let row_placeholders: Vec<&str> = acks.iter().map(|_| "(?, ?)").collect();
        let sql = format!(
            "INSERT OR REPLACE INTO sync_checkpoints (entity_type, ack) VALUES {}",
            row_placeholders.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for (entity_type, ack) in acks {
            query = query.bind(entity_type).bind(ack);
        }
        query
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Drop every checkpoint row. Used when the server signals a reset
    /// sync — the next pull starts from the beginning of the stream.
    pub async fn clear_checkpoints(&self) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM sync_checkpoints")
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    // ── Audit log ───────────────────────────────────────────────────

    /// Record the start of processing one sync record. Returns the row
    /// id, which the caller passes to [`complete_audit`] / [`fail_audit`]
    /// once the line is settled.
    ///
    /// The row is inserted with `action = 'started'` as a sentinel.
    /// `complete_audit` overwrites it with the actual action ("upsert" /
    /// "delete" / "reset" / …) and `fail_audit` overwrites it with
    /// "error". A row left on `'started'` is the post-mortem signal for
    /// a crash between `start_audit` and the settling call.
    pub async fn start_audit(
        &self,
        entity_type: &str,
        entity_id: &str,
        sync_cycle: &str,
    ) -> Result<i64, LibraryError> {
        let now = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO sync_audit (entity_type, entity_id, action, started_at, sync_cycle)
             VALUES (?, ?, 'started', ?, ?)",
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(&now)
        .bind(sync_cycle)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(result.last_insert_rowid())
    }

    /// Mark a previously-started audit row as completed with the given action.
    pub async fn complete_audit(&self, row_id: i64, action: &str) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE sync_audit SET completed_at = ?, action = ? WHERE id = ?")
            .bind(&now)
            .bind(action)
            .bind(row_id)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Mark a previously-started audit row as errored with a message.
    pub async fn fail_audit(&self, row_id: i64, error_msg: &str) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE sync_audit SET completed_at = ?, action = 'error', error_msg = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(error_msg)
        .bind(row_id)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Determine whether a reset reconciliation is in progress, and
    /// return its checkpoint timestamp if so.
    ///
    /// Issue #628: looks at the most recent successfully-handled
    /// `SyncResetV1` / `SyncCompleteV1` audit row. If the latest one
    /// is `SyncResetV1` (action `'reset'`), a reset cycle started
    /// but hasn't been closed by a `SyncCompleteV1` — return
    /// `Some(started_at_unix_seconds)` so the dispatch loop can
    /// resume in reset mode. This covers the case where a previous
    /// stream disconnected mid-reset and the server resumes from its
    /// last ack rather than re-issuing `SyncResetV1`.
    ///
    /// The filter `action IN ('reset', 'complete')` excludes rows
    /// whose dispatch crashed before completion (`'started'`) and
    /// rows whose handler errored (`'error'`). Both should leave the
    /// state machine in whatever mode the prior settled row implies.
    pub async fn current_reset_checkpoint(&self) -> Result<Option<i64>, LibraryError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT entity_type, started_at FROM sync_audit
             WHERE entity_type IN ('SyncResetV1', 'SyncCompleteV1')
               AND action IN ('reset', 'complete')
             ORDER BY id DESC
             LIMIT 1",
        )
        .fetch_optional(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        match row {
            Some((entity_type, started_at)) if entity_type == "SyncResetV1" => {
                let dt = chrono::DateTime::parse_from_rfc3339(&started_at).map_err(|e| {
                    LibraryError::Immich(format!(
                        "current_reset_checkpoint: invalid started_at {started_at:?}: {e}"
                    ))
                })?;
                Ok(Some(dt.timestamp()))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn open_db() -> (tempfile::TempDir, Database) {
        let dir = tempdir().unwrap();
        let db = Database::new();
        db.open(&dir.path().join("test.db")).await.unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn audit_start_and_complete() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        let row_id = repo
            .start_audit("AssetV1", "uuid-1", "cycle-1")
            .await
            .unwrap();
        assert!(row_id > 0);

        repo.complete_audit(row_id, "upsert").await.unwrap();

        let row: (String, Option<String>) =
            sqlx::query_as("SELECT action, completed_at FROM sync_audit WHERE id = ?")
                .bind(row_id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "upsert");
        assert!(row.1.is_some());
    }

    #[tokio::test]
    async fn audit_fail_records_error_message() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        let row_id = repo
            .start_audit("AssetV1", "uuid-fail", "cycle-2")
            .await
            .unwrap();

        repo.fail_audit(row_id, "parse error").await.unwrap();

        let row: (String, Option<String>) =
            sqlx::query_as("SELECT action, error_msg FROM sync_audit WHERE id = ?")
                .bind(row_id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "error");
        assert_eq!(row.1.as_deref(), Some("parse error"));
    }

    #[tokio::test]
    async fn checkpoints_save_and_clear() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        let pairs = vec![
            ("AssetV1".to_string(), "ack-asset-100".to_string()),
            ("AlbumV1".to_string(), "ack-album-50".to_string()),
        ];
        repo.save_checkpoints(&pairs).await.unwrap();

        let row: (String,) =
            sqlx::query_as("SELECT ack FROM sync_checkpoints WHERE entity_type = 'AssetV1'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "ack-asset-100");

        repo.clear_checkpoints().await.unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sync_checkpoints")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn checkpoints_upsert_replaces_existing_ack() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        repo.save_checkpoints(&[("AssetV1".to_string(), "ack-1".to_string())])
            .await
            .unwrap();
        repo.save_checkpoints(&[("AssetV1".to_string(), "ack-2".to_string())])
            .await
            .unwrap();

        let row: (String,) =
            sqlx::query_as("SELECT ack FROM sync_checkpoints WHERE entity_type = 'AssetV1'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "ack-2");
    }

    #[tokio::test]
    async fn save_checkpoints_empty_is_noop() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        repo.save_checkpoints(&[]).await.unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sync_checkpoints")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn current_reset_checkpoint_none_when_no_audit_rows() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());
        assert_eq!(repo.current_reset_checkpoint().await.unwrap(), None);
    }

    #[tokio::test]
    async fn current_reset_checkpoint_some_when_latest_is_reset() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        let row_id = repo
            .start_audit("SyncResetV1", "", "cycle-1")
            .await
            .unwrap();
        repo.complete_audit(row_id, "reset").await.unwrap();

        let result = repo.current_reset_checkpoint().await.unwrap();
        assert!(result.is_some(), "expected checkpoint after SyncResetV1");
    }

    #[tokio::test]
    async fn current_reset_checkpoint_none_when_latest_is_complete() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        let reset_id = repo
            .start_audit("SyncResetV1", "", "cycle-1")
            .await
            .unwrap();
        repo.complete_audit(reset_id, "reset").await.unwrap();
        let complete_id = repo
            .start_audit("SyncCompleteV1", "", "cycle-1")
            .await
            .unwrap();
        repo.complete_audit(complete_id, "complete").await.unwrap();

        // Reset was closed by SyncCompleteV1; no longer in reset mode.
        assert_eq!(repo.current_reset_checkpoint().await.unwrap(), None);
    }

    #[tokio::test]
    async fn current_reset_checkpoint_excludes_started_but_uncompleted() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        // start_audit was called but neither complete_audit nor
        // fail_audit followed — handler was still running when we
        // crashed. action stays 'started', completed_at is NULL.
        let _row_id = repo
            .start_audit("SyncResetV1", "", "cycle-1")
            .await
            .unwrap();

        // We can't trust that this cycle made any progress; treat as
        // not-in-reset-mode. A future SyncResetV1 will start fresh.
        assert_eq!(repo.current_reset_checkpoint().await.unwrap(), None);
    }

    #[tokio::test]
    async fn current_reset_checkpoint_excludes_errored_resets() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        let row_id = repo
            .start_audit("SyncResetV1", "", "cycle-1")
            .await
            .unwrap();
        repo.fail_audit(row_id, "boom").await.unwrap();

        // An errored reset shouldn't pin us in reset mode.
        assert_eq!(repo.current_reset_checkpoint().await.unwrap(), None);
    }

    #[tokio::test]
    async fn current_reset_checkpoint_ignores_other_entity_types() {
        let (_dir, db) = open_db().await;
        let repo = SyncStateRepository::new(db.clone());

        // Most recent reset/complete is the reset; AssetV1 rows after
        // it (the dispatch in the resumed stream) shouldn't shift us
        // out of reset mode.
        let reset_id = repo
            .start_audit("SyncResetV1", "", "cycle-1")
            .await
            .unwrap();
        repo.complete_audit(reset_id, "reset").await.unwrap();
        for _ in 0..3 {
            let aid = repo.start_audit("AssetV1", "", "cycle-1").await.unwrap();
            repo.complete_audit(aid, "upsert").await.unwrap();
        }

        assert!(
            repo.current_reset_checkpoint().await.unwrap().is_some(),
            "AssetV1 audit rows must not close the reset cycle"
        );
    }
}

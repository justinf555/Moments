// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Outbox table persistence.

use tracing::{debug, info, instrument};

use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::mutation::OutboxRow;

/// Outbox entry lifecycle status stored in the `status` column.
///
/// Shared between the push manager and the preferences/admin path so
/// there is exactly one source of truth for the integer encoding.
#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxStatus {
    Pending = 0,
    Done = 1,
    Failed = 2,
    /// Terminal — exceeded the push manager's max attempts. Never
    /// retried automatically; the user can reset via the preferences UI.
    DeadLetter = 3,
}

/// Counts of outbox entries by status, for the preferences UI.
///
/// `done` is omitted — rows in that state are purged each push cycle,
/// so the count is uninteresting from the user's perspective.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboxCounts {
    pub pending: u64,
    pub failed: u64,
    pub dead_letter: u64,
}

/// Repository for the `sync_outbox` table.
#[derive(Clone)]
pub struct OutboxRepository {
    db: Database,
}

impl OutboxRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Insert a single outbox row.
    #[instrument(skip(self))]
    pub async fn insert(&self, row: &OutboxRow) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO sync_outbox (entity_type, entity_id, action, payload, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&row.entity_type)
        .bind(&row.entity_id)
        .bind(&row.action)
        .bind(row.payload.as_deref())
        .bind(now)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        debug!(
            entity_type = %row.entity_type,
            entity_id = %row.entity_id,
            action = %row.action,
            "outbox entry queued"
        );
        Ok(())
    }

    /// Counts of entries by status. One round trip — three conditional
    /// sums beats three separate queries for a UI that just rendered.
    #[instrument(skip(self))]
    pub async fn count_by_status(&self) -> Result<OutboxCounts, LibraryError> {
        let row: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                COALESCE(SUM(status = ?), 0),
                COALESCE(SUM(status = ?), 0),
                COALESCE(SUM(status = ?), 0)
             FROM sync_outbox",
        )
        .bind(OutboxStatus::Pending as i64)
        .bind(OutboxStatus::Failed as i64)
        .bind(OutboxStatus::DeadLetter as i64)
        .fetch_one(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(OutboxCounts {
            pending: row.0.max(0) as u64,
            failed: row.1.max(0) as u64,
            dead_letter: row.2.max(0) as u64,
        })
    }

    /// Reset every `Failed` row so the push loop tries it immediately.
    /// Clears the attempts counter, last error, and backoff schedule.
    /// Returns the number of rows reset.
    ///
    /// Dead-letter rows are not affected — use [`Self::clear_dead_letters`]
    /// to remove them, or insert a fresh mutation to retry the action.
    #[instrument(skip(self))]
    pub async fn retry_failed(&self) -> Result<u64, LibraryError> {
        let result = sqlx::query(
            "UPDATE sync_outbox
             SET status = ?, attempts = 0, last_error = NULL, next_attempt_at = 0
             WHERE status = ?",
        )
        .bind(OutboxStatus::Pending as i64)
        .bind(OutboxStatus::Failed as i64)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        let n = result.rows_affected();
        info!(rows = n, "reset failed outbox entries for retry");
        Ok(n)
    }

    /// Permanently delete dead-letter entries. Returns the number of
    /// rows removed.
    #[instrument(skip(self))]
    pub async fn clear_dead_letters(&self) -> Result<u64, LibraryError> {
        let result = sqlx::query("DELETE FROM sync_outbox WHERE status = ?")
            .bind(OutboxStatus::DeadLetter as i64)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;

        let n = result.rows_affected();
        info!(rows = n, "cleared dead-letter outbox entries");
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::media::MediaId;
    use crate::library::mutation::Mutation;

    async fn open_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new();
        db.open(&dir.path().join("test.db")).await.unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn insert_stores_row_with_timestamp() {
        let (_dir, db) = open_db().await;
        let repo = OutboxRepository::new(db.clone());

        let rows = Mutation::AssetTrashed {
            ids: vec![MediaId::new("t1".to_string())],
        }
        .to_outbox_rows();

        let before = chrono::Utc::now().timestamp();
        repo.insert(&rows[0]).await.unwrap();
        let after = chrono::Utc::now().timestamp();

        let row: (String, String, String, i64) = sqlx::query_as(
            "SELECT entity_type, entity_id, action, created_at FROM sync_outbox WHERE id = 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();

        assert_eq!(row.0, "asset");
        assert_eq!(row.1, "t1");
        assert_eq!(row.2, "trash");
        assert!(row.3 >= before);
        assert!(row.3 <= after);
    }

    /// Insert a row with a specific status — used by the admin tests.
    async fn insert_with_status(db: &Database, entity_id: &str, status: OutboxStatus) {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO sync_outbox
                (entity_type, entity_id, action, payload, created_at, status)
             VALUES ('asset', ?, 'trash', NULL, ?, ?)",
        )
        .bind(entity_id)
        .bind(now)
        .bind(status as i64)
        .execute(db.pool())
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn count_by_status_returns_each_bucket() {
        let (_dir, db) = open_db().await;
        let repo = OutboxRepository::new(db.clone());

        insert_with_status(&db, "p1", OutboxStatus::Pending).await;
        insert_with_status(&db, "p2", OutboxStatus::Pending).await;
        insert_with_status(&db, "f1", OutboxStatus::Failed).await;
        insert_with_status(&db, "d1", OutboxStatus::DeadLetter).await;
        insert_with_status(&db, "d2", OutboxStatus::DeadLetter).await;
        insert_with_status(&db, "d3", OutboxStatus::DeadLetter).await;
        // Done rows must not appear in any of the user-facing buckets.
        insert_with_status(&db, "done1", OutboxStatus::Done).await;

        let counts = repo.count_by_status().await.unwrap();
        assert_eq!(counts.pending, 2);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.dead_letter, 3);
    }

    #[tokio::test]
    async fn count_by_status_empty_table_returns_zeros() {
        let (_dir, db) = open_db().await;
        let repo = OutboxRepository::new(db);

        let counts = repo.count_by_status().await.unwrap();
        assert_eq!(counts, OutboxCounts::default());
    }

    #[tokio::test]
    async fn retry_failed_resets_only_failed_rows() {
        let (_dir, db) = open_db().await;
        let repo = OutboxRepository::new(db.clone());

        insert_with_status(&db, "p1", OutboxStatus::Pending).await;
        insert_with_status(&db, "f1", OutboxStatus::Failed).await;
        insert_with_status(&db, "f2", OutboxStatus::Failed).await;
        insert_with_status(&db, "d1", OutboxStatus::DeadLetter).await;

        // Give the failed rows non-trivial retry state to confirm reset.
        sqlx::query(
            "UPDATE sync_outbox
             SET attempts = 7, last_error = 'boom', next_attempt_at = 9_999_999_999
             WHERE status = ?",
        )
        .bind(OutboxStatus::Failed as i64)
        .execute(db.pool())
        .await
        .unwrap();

        let n = repo.retry_failed().await.unwrap();
        assert_eq!(n, 2);

        // Failed rows are now Pending with cleared retry state.
        let resets: Vec<(String, i64, i64, Option<String>, i64)> = sqlx::query_as(
            "SELECT entity_id, status, attempts, last_error, next_attempt_at
             FROM sync_outbox WHERE entity_id IN ('f1', 'f2')
             ORDER BY entity_id",
        )
        .fetch_all(db.pool())
        .await
        .unwrap();
        for r in resets {
            assert_eq!(r.1, OutboxStatus::Pending as i64);
            assert_eq!(r.2, 0);
            assert!(r.3.is_none());
            assert_eq!(r.4, 0);
        }

        // Dead-letter row is untouched.
        let d: (i64,) = sqlx::query_as("SELECT status FROM sync_outbox WHERE entity_id = 'd1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(d.0, OutboxStatus::DeadLetter as i64);
    }

    #[tokio::test]
    async fn clear_dead_letters_only_removes_dead_letter_rows() {
        let (_dir, db) = open_db().await;
        let repo = OutboxRepository::new(db.clone());

        insert_with_status(&db, "p1", OutboxStatus::Pending).await;
        insert_with_status(&db, "f1", OutboxStatus::Failed).await;
        insert_with_status(&db, "d1", OutboxStatus::DeadLetter).await;
        insert_with_status(&db, "d2", OutboxStatus::DeadLetter).await;

        let n = repo.clear_dead_letters().await.unwrap();
        assert_eq!(n, 2);

        let remaining: Vec<(String,)> =
            sqlx::query_as("SELECT entity_id FROM sync_outbox ORDER BY entity_id")
                .fetch_all(db.pool())
                .await
                .unwrap();
        let ids: Vec<&str> = remaining.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(ids, vec!["f1", "p1"]);
    }

    #[tokio::test]
    async fn insert_stores_payload() {
        let (_dir, db) = open_db().await;
        let repo = OutboxRepository::new(db.clone());

        let rows = Mutation::AlbumCreated {
            id: crate::library::album::AlbumId::from_raw("a1".to_string()),
            name: "Vacation".to_string(),
        }
        .to_outbox_rows();

        repo.insert(&rows[0]).await.unwrap();

        let row: (Option<String>,) = sqlx::query_as("SELECT payload FROM sync_outbox WHERE id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();

        let payload: serde_json::Value = serde_json::from_str(row.0.as_deref().unwrap()).unwrap();
        assert_eq!(payload["name"], "Vacation");
    }
}

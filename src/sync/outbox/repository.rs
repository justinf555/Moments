//! Outbox table persistence.

use tracing::{debug, instrument};

use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::mutation::OutboxRow;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::mutation::Mutation;
    use crate::library::media::MediaId;

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

        let row: (Option<String>,) =
            sqlx::query_as("SELECT payload FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();

        let payload: serde_json::Value =
            serde_json::from_str(row.0.as_deref().unwrap()).unwrap();
        assert_eq!(payload["name"], "Vacation");
    }
}

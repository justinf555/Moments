//! [`MutationRecorder`] implementations.
//!
//! - [`NoOpRecorder`] — local backend, does nothing.
//! - [`QueueWriterOutbox`] — Immich backend, writes to the `sync_outbox` table.

use async_trait::async_trait;
use tracing::{debug, instrument};

use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;

// ── NoOpRecorder ──────────────────────────────────────────────────────

/// Does nothing. Used by the local backend where mutations stay local.
pub struct NoOpRecorder;

#[async_trait]
impl MutationRecorder for NoOpRecorder {
    async fn record(&self, _mutation: &Mutation) -> Result<(), LibraryError> {
        Ok(())
    }
}

// ── QueueWriterOutbox ─────────────────────────────────────────────────

/// Writes mutations to the `sync_outbox` table for later push to Immich.
pub struct QueueWriterOutbox {
    db: Database,
}

impl QueueWriterOutbox {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Insert one or more outbox rows for the given mutation.
    #[instrument(skip(self))]
    async fn enqueue(
        &self,
        entity_type: &str,
        entity_id: &str,
        action: &str,
        payload: Option<&str>,
    ) -> Result<(), LibraryError> {
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
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        debug!(entity_type, entity_id, action, "outbox entry queued");
        Ok(())
    }
}

#[async_trait]
impl MutationRecorder for QueueWriterOutbox {
    async fn record(&self, mutation: &Mutation) -> Result<(), LibraryError> {
        match mutation {
            // ── Asset ────────────────────────────────────────────────
            Mutation::AssetImported { id, file_path } => {
                let payload = serde_json::json!({
                    "file_path": file_path.to_string_lossy(),
                })
                .to_string();
                self.enqueue("asset", id.as_str(), "import", Some(&payload))
                    .await
            }

            Mutation::AssetFavorited { ids, favorite } => {
                let action = if *favorite { "favorite" } else { "unfavorite" };
                for id in ids {
                    self.enqueue("asset", id.as_str(), action, None).await?;
                }
                Ok(())
            }

            Mutation::AssetTrashed { ids } => {
                for id in ids {
                    self.enqueue("asset", id.as_str(), "trash", None).await?;
                }
                Ok(())
            }

            Mutation::AssetRestored { ids } => {
                for id in ids {
                    self.enqueue("asset", id.as_str(), "restore", None).await?;
                }
                Ok(())
            }

            Mutation::AssetDeleted { ids } => {
                for id in ids {
                    self.enqueue("asset", id.as_str(), "delete", None).await?;
                }
                Ok(())
            }

            // ── Album ────────────────────────────────────────────────
            Mutation::AlbumCreated { id, name } => {
                let payload = serde_json::json!({ "name": name }).to_string();
                self.enqueue("album", id.as_str(), "create", Some(&payload))
                    .await
            }

            Mutation::AlbumRenamed { id, name } => {
                let payload = serde_json::json!({ "name": name }).to_string();
                self.enqueue("album", id.as_str(), "rename", Some(&payload))
                    .await
            }

            Mutation::AlbumDeleted { id } => {
                self.enqueue("album", id.as_str(), "delete", None).await
            }

            Mutation::AlbumMediaAdded {
                album_id,
                media_ids,
            } => {
                let payload = serde_json::json!({
                    "media_ids": media_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                })
                .to_string();
                self.enqueue("album", album_id.as_str(), "add_media", Some(&payload))
                    .await
            }

            Mutation::AlbumMediaRemoved {
                album_id,
                media_ids,
            } => {
                let payload = serde_json::json!({
                    "media_ids": media_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                })
                .to_string();
                self.enqueue("album", album_id.as_str(), "remove_media", Some(&payload))
                    .await
            }

            // ── People ──────────────────────────────────────────────
            Mutation::PersonRenamed { id, name } => {
                let payload = serde_json::json!({ "name": name }).to_string();
                self.enqueue("person", id.as_str(), "rename", Some(&payload))
                    .await
            }

            Mutation::PersonHidden { id, hidden } => {
                let payload = serde_json::json!({ "hidden": hidden }).to_string();
                self.enqueue("person", id.as_str(), "hide", Some(&payload))
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::media::MediaId;

    #[tokio::test]
    async fn noop_recorder_returns_ok() {
        let recorder = NoOpRecorder;
        let result = recorder
            .record(&Mutation::AssetTrashed {
                ids: vec![MediaId::new("test".to_string())],
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn queue_writer_enqueues_favorite() {
        let dir = tempfile::tempdir().unwrap();
        let db = {
            let db = Database::new();
            db.open(&dir.path().join("test.db")).await.unwrap();
            db
        };
        let writer = QueueWriterOutbox::new(db.clone());

        let id = MediaId::new("abc123".to_string());
        writer
            .record(&Mutation::AssetFavorited {
                ids: vec![id],
                favorite: true,
            })
            .await
            .unwrap();

        let row: (String, String, String, i64) = sqlx::query_as(
            "SELECT entity_type, entity_id, action, status FROM sync_outbox WHERE id = 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();

        assert_eq!(row.0, "asset");
        assert_eq!(row.1, "abc123");
        assert_eq!(row.2, "favorite");
        assert_eq!(row.3, 0); // pending
    }

    #[tokio::test]
    async fn queue_writer_enqueues_album_with_payload() {
        let dir = tempfile::tempdir().unwrap();
        let db = {
            let db = Database::new();
            db.open(&dir.path().join("test.db")).await.unwrap();
            db
        };
        let writer = QueueWriterOutbox::new(db.clone());

        let album_id = crate::library::album::AlbumId::new();
        writer
            .record(&Mutation::AlbumCreated {
                id: album_id.clone(),
                name: "Vacation".to_string(),
            })
            .await
            .unwrap();

        let row: (String, String, Option<String>) =
            sqlx::query_as("SELECT entity_type, action, payload FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();

        assert_eq!(row.0, "album");
        assert_eq!(row.1, "create");
        let payload: serde_json::Value = serde_json::from_str(row.2.as_deref().unwrap()).unwrap();
        assert_eq!(payload["name"], "Vacation");
    }

    #[tokio::test]
    async fn queue_writer_enqueues_per_id_for_batch() {
        let dir = tempfile::tempdir().unwrap();
        let db = {
            let db = Database::new();
            db.open(&dir.path().join("test.db")).await.unwrap();
            db
        };
        let writer = QueueWriterOutbox::new(db.clone());

        writer
            .record(&Mutation::AssetTrashed {
                ids: vec![
                    MediaId::new("a".to_string()),
                    MediaId::new("b".to_string()),
                    MediaId::new("c".to_string()),
                ],
            })
            .await
            .unwrap();

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sync_outbox")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 3);
    }
}

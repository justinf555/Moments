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

    // ── Additional outbox tests ──────────────────────────────────────

    async fn open_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new();
        db.open(&dir.path().join("test.db")).await.unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn queue_writer_enqueues_unfavorite() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());

        writer
            .record(&Mutation::AssetFavorited {
                ids: vec![MediaId::new("x".to_string())],
                favorite: false,
            })
            .await
            .unwrap();

        let row: (String,) =
            sqlx::query_as("SELECT action FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "unfavorite");
    }

    #[tokio::test]
    async fn queue_writer_enqueues_asset_imported() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());

        writer
            .record(&Mutation::AssetImported {
                id: MediaId::new("img001".to_string()),
                file_path: std::path::PathBuf::from("/photos/test.jpg"),
            })
            .await
            .unwrap();

        let row: (String, String, String, Option<String>) = sqlx::query_as(
            "SELECT entity_type, entity_id, action, payload FROM sync_outbox WHERE id = 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();

        assert_eq!(row.0, "asset");
        assert_eq!(row.1, "img001");
        assert_eq!(row.2, "import");
        let payload: serde_json::Value = serde_json::from_str(row.3.as_deref().unwrap()).unwrap();
        assert_eq!(payload["file_path"], "/photos/test.jpg");
    }

    #[tokio::test]
    async fn queue_writer_enqueues_asset_restored() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());

        writer
            .record(&Mutation::AssetRestored {
                ids: vec![
                    MediaId::new("r1".to_string()),
                    MediaId::new("r2".to_string()),
                ],
            })
            .await
            .unwrap();

        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT entity_id, action FROM sync_outbox ORDER BY id")
                .fetch_all(db.pool())
                .await
                .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "r1");
        assert_eq!(rows[0].1, "restore");
        assert_eq!(rows[1].0, "r2");
        assert_eq!(rows[1].1, "restore");
    }

    #[tokio::test]
    async fn queue_writer_enqueues_asset_deleted() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());

        writer
            .record(&Mutation::AssetDeleted {
                ids: vec![MediaId::new("del1".to_string())],
            })
            .await
            .unwrap();

        let row: (String, String) =
            sqlx::query_as("SELECT entity_type, action FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(row.0, "asset");
        assert_eq!(row.1, "delete");
    }

    #[tokio::test]
    async fn queue_writer_enqueues_album_renamed() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());
        let album_id = crate::library::album::AlbumId::from_raw("album-1".to_string());

        writer
            .record(&Mutation::AlbumRenamed {
                id: album_id,
                name: "New Name".to_string(),
            })
            .await
            .unwrap();

        let row: (String, String, Option<String>) =
            sqlx::query_as("SELECT entity_type, action, payload FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();

        assert_eq!(row.0, "album");
        assert_eq!(row.1, "rename");
        let payload: serde_json::Value = serde_json::from_str(row.2.as_deref().unwrap()).unwrap();
        assert_eq!(payload["name"], "New Name");
    }

    #[tokio::test]
    async fn queue_writer_enqueues_album_deleted() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());
        let album_id = crate::library::album::AlbumId::from_raw("album-del".to_string());

        writer
            .record(&Mutation::AlbumDeleted { id: album_id })
            .await
            .unwrap();

        let row: (String, String, String, Option<String>) = sqlx::query_as(
            "SELECT entity_type, entity_id, action, payload FROM sync_outbox WHERE id = 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();

        assert_eq!(row.0, "album");
        assert_eq!(row.1, "album-del");
        assert_eq!(row.2, "delete");
        assert!(row.3.is_none());
    }

    #[tokio::test]
    async fn queue_writer_enqueues_album_media_added() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());
        let album_id = crate::library::album::AlbumId::from_raw("album-x".to_string());

        writer
            .record(&Mutation::AlbumMediaAdded {
                album_id,
                media_ids: vec![
                    MediaId::new("m1".to_string()),
                    MediaId::new("m2".to_string()),
                ],
            })
            .await
            .unwrap();

        let row: (String, String, Option<String>) =
            sqlx::query_as("SELECT entity_id, action, payload FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();

        assert_eq!(row.0, "album-x");
        assert_eq!(row.1, "add_media");
        let payload: serde_json::Value = serde_json::from_str(row.2.as_deref().unwrap()).unwrap();
        let ids = payload["media_ids"].as_array().unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], "m1");
        assert_eq!(ids[1], "m2");
    }

    #[tokio::test]
    async fn queue_writer_enqueues_album_media_removed() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());
        let album_id = crate::library::album::AlbumId::from_raw("album-y".to_string());

        writer
            .record(&Mutation::AlbumMediaRemoved {
                album_id,
                media_ids: vec![MediaId::new("m3".to_string())],
            })
            .await
            .unwrap();

        let row: (String, Option<String>) =
            sqlx::query_as("SELECT action, payload FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();

        assert_eq!(row.0, "remove_media");
        let payload: serde_json::Value = serde_json::from_str(row.1.as_deref().unwrap()).unwrap();
        assert_eq!(payload["media_ids"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn queue_writer_enqueues_person_renamed() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());
        let person_id = crate::library::faces::PersonId::from_raw("person-1".to_string());

        writer
            .record(&Mutation::PersonRenamed {
                id: person_id,
                name: "Alice".to_string(),
            })
            .await
            .unwrap();

        let row: (String, String, String, Option<String>) = sqlx::query_as(
            "SELECT entity_type, entity_id, action, payload FROM sync_outbox WHERE id = 1",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();

        assert_eq!(row.0, "person");
        assert_eq!(row.1, "person-1");
        assert_eq!(row.2, "rename");
        let payload: serde_json::Value = serde_json::from_str(row.3.as_deref().unwrap()).unwrap();
        assert_eq!(payload["name"], "Alice");
    }

    #[tokio::test]
    async fn queue_writer_enqueues_person_hidden() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());
        let person_id = crate::library::faces::PersonId::from_raw("person-2".to_string());

        writer
            .record(&Mutation::PersonHidden {
                id: person_id,
                hidden: true,
            })
            .await
            .unwrap();

        let row: (String, String, Option<String>) =
            sqlx::query_as("SELECT entity_type, action, payload FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();

        assert_eq!(row.0, "person");
        assert_eq!(row.1, "hide");
        let payload: serde_json::Value = serde_json::from_str(row.2.as_deref().unwrap()).unwrap();
        assert_eq!(payload["hidden"], true);
    }

    #[tokio::test]
    async fn queue_writer_enqueues_person_unhidden() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());
        let person_id = crate::library::faces::PersonId::from_raw("person-3".to_string());

        writer
            .record(&Mutation::PersonHidden {
                id: person_id,
                hidden: false,
            })
            .await
            .unwrap();

        let row: (Option<String>,) =
            sqlx::query_as("SELECT payload FROM sync_outbox WHERE id = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();

        let payload: serde_json::Value = serde_json::from_str(row.0.as_deref().unwrap()).unwrap();
        assert_eq!(payload["hidden"], false);
    }

    #[tokio::test]
    async fn queue_writer_sets_created_at_timestamp() {
        let (_dir, db) = open_db().await;
        let writer = QueueWriterOutbox::new(db.clone());

        let before = chrono::Utc::now().timestamp();
        writer
            .record(&Mutation::AssetTrashed {
                ids: vec![MediaId::new("t1".to_string())],
            })
            .await
            .unwrap();
        let after = chrono::Utc::now().timestamp();

        let row: (i64,) = sqlx::query_as("SELECT created_at FROM sync_outbox WHERE id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();

        assert!(row.0 >= before);
        assert!(row.0 <= after);
    }
}

use crate::library::error::LibraryError;
use crate::library::media::repository::MediaRepository;
use crate::library::media::MediaRecord;
use crate::library::metadata::MediaMetadataRecord;

use super::Database;

impl Database {
    /// Forwarding shim — delegates to `MediaRepository`.
    pub async fn upsert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        MediaRepository::new(self.clone()).upsert(record).await
    }

    /// Forwarding shim — delegates to `MetadataRepository`.
    pub async fn upsert_media_metadata(
        &self,
        record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        crate::library::metadata::repository::MetadataRepository::new(self.clone())
            .upsert(record)
            .await
    }

    /// Upsert an album-media association from the sync stream.
    pub async fn upsert_album_media(
        &self,
        album_id: &str,
        media_id: &str,
        added_at: i64,
    ) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT OR IGNORE INTO album_media (album_id, media_id, added_at) VALUES (?, ?, ?)",
        )
        .bind(album_id)
        .bind(media_id)
        .bind(added_at)
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Delete a single album-media association.
    pub async fn delete_album_media_entry(
        &self,
        album_id: &str,
        media_id: &str,
    ) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM album_media WHERE album_id = ? AND media_id = ?")
            .bind(album_id)
            .bind(media_id)
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Load all media IDs into a HashSet (for reset sync deletion detection).
    pub async fn all_media_ids(&self) -> Result<std::collections::HashSet<String>, LibraryError> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM media")
            .fetch_all(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Save sync checkpoints (batch upsert).
    pub async fn save_sync_checkpoints(
        &self,
        acks: &[(String, String)],
    ) -> Result<(), LibraryError> {
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
        query.execute(&self.pool).await.map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Clear all sync checkpoints (for reset sync).
    pub async fn clear_sync_checkpoints(&self) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM sync_checkpoints")
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Record the start of processing a sync record.
    /// Returns the row id for later completion via [`complete_sync_audit`].
    pub async fn start_sync_audit(
        &self,
        entity_type: &str,
        entity_id: &str,
        sync_cycle: &str,
    ) -> Result<i64, LibraryError> {
        let now = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO sync_audit (entity_type, entity_id, action, started_at, sync_cycle)
             VALUES (?, ?, 'upsert', ?, ?)",
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(&now)
        .bind(sync_cycle)
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(result.last_insert_rowid())
    }

    /// Mark a sync audit record as completed (just before acking).
    pub async fn complete_sync_audit(&self, row_id: i64, action: &str) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE sync_audit SET completed_at = ?, action = ? WHERE id = ?")
            .bind(&now)
            .bind(action)
            .bind(row_id)
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Mark a sync audit record as failed with an error message.
    pub async fn fail_sync_audit(&self, row_id: i64, error_msg: &str) -> Result<(), LibraryError> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE sync_audit SET completed_at = ?, action = 'error', error_msg = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(error_msg)
        .bind(row_id)
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }
}

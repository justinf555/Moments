use crate::library::error::LibraryError;
use crate::library::media::{MediaId, MediaRecord};

use super::{id_placeholders, Database};

impl Database {
    pub(crate) async fn insert_media_record(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT INTO media (id, relative_path, original_filename, file_size,
                                imported_at, media_type, taken_at, width, height,
                                orientation, duration_ms, is_favorite, is_trashed,
                                trashed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id.as_str())
        .bind(&record.relative_path)
        .bind(&record.original_filename)
        .bind(record.file_size)
        .bind(record.imported_at)
        .bind(record.media_type as i64)
        .bind(record.taken_at)
        .bind(record.width)
        .bind(record.height)
        .bind(record.orientation as i64)
        .bind(record.duration_ms.map(|v| v as i64))
        .bind(record.is_favorite as i64)
        .bind(record.is_trashed as i64)
        .bind(record.trashed_at)
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    pub(crate) async fn set_favorite_ids(
        &self,
        ids: &[MediaId],
        favorite: bool,
    ) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let value: i64 = if favorite { 1 } else { 0 };
        let placeholders = id_placeholders(ids.len());
        let sql = format!("UPDATE media SET is_favorite = ? WHERE id IN ({placeholders})");
        let mut query = sqlx::query(&sql);
        query = query.bind(value);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query.execute(&self.pool).await.map_err(LibraryError::Db)?;
        Ok(())
    }

    pub(crate) async fn trash_ids(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        let placeholders = id_placeholders(ids.len());
        let sql = format!(
            "UPDATE media SET is_trashed = 1, trashed_at = ? WHERE id IN ({placeholders})"
        );
        let mut query = sqlx::query(&sql);
        query = query.bind(now);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query.execute(&self.pool).await.map_err(LibraryError::Db)?;
        Ok(())
    }

    pub(crate) async fn restore_ids(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = id_placeholders(ids.len());
        let sql = format!(
            "UPDATE media SET is_trashed = 0, trashed_at = NULL WHERE id IN ({placeholders})"
        );
        let mut query = sqlx::query(&sql);
        for id in ids {
            query = query.bind(id.as_str());
        }
        query.execute(&self.pool).await.map_err(LibraryError::Db)?;
        Ok(())
    }

    pub(crate) async fn delete_permanently_ids(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = id_placeholders(ids.len());
        for (table, col) in [
            ("media_metadata", "media_id"),
            ("thumbnails", "media_id"),
            ("album_media", "media_id"),
            ("media", "id"),
        ] {
            let sql = format!("DELETE FROM {table} WHERE {col} IN ({placeholders})");
            let mut query = sqlx::query(&sql);
            for id in ids {
                query = query.bind(id.as_str());
            }
            query.execute(&self.pool).await.map_err(LibraryError::Db)?;
        }
        Ok(())
    }
}

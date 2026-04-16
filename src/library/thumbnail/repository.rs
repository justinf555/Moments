use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

use super::model::ThumbnailStatus;

/// Thumbnail persistence layer.
///
/// Encapsulates all thumbnail status SQL queries.
#[derive(Clone)]
pub struct ThumbnailRepository {
    db: Database,
}

impl ThumbnailRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Insert a `Pending` thumbnail row. No-op if a row already exists.
    pub async fn insert_pending(&self, id: &MediaId) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        sqlx::query("INSERT OR IGNORE INTO thumbnails (media_id, status) VALUES (?, 0)")
            .bind(id_str)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Mark a thumbnail `Ready` and record its relative `file_path`.
    pub async fn set_ready(
        &self,
        id: &MediaId,
        file_path: &str,
        generated_at: i64,
    ) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        sqlx::query(
            "UPDATE thumbnails SET status = 1, file_path = ?, generated_at = ? WHERE media_id = ?",
        )
        .bind(file_path)
        .bind(generated_at)
        .bind(id_str)
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Mark a thumbnail `Failed`.
    pub async fn set_failed(&self, id: &MediaId) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        sqlx::query("UPDATE thumbnails SET status = 2 WHERE media_id = ?")
            .bind(id_str)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Return the stored [`ThumbnailStatus`] for `id`, or `None` if no row exists.
    pub async fn status(&self, id: &MediaId) -> Result<Option<ThumbnailStatus>, LibraryError> {
        let id_str = id.as_str();
        let row: Option<i64> =
            sqlx::query_scalar("SELECT status FROM thumbnails WHERE media_id = ?")
                .bind(id_str)
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(row.map(ThumbnailStatus::from_i64))
    }
}

use crate::library::error::LibraryError;

use super::{Database, LibraryStats};

impl Database {
    /// Return aggregate library statistics for the preferences overview.
    pub async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        let row: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                COUNT(CASE WHEN media_type = 0 AND is_trashed = 0 THEN 1 END),
                COUNT(CASE WHEN media_type = 1 AND is_trashed = 0 THEN 1 END),
                COALESCE(SUM(CASE WHEN is_trashed = 0 THEN file_size ELSE 0 END), 0)
             FROM media",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(LibraryError::Db)?;

        let album_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM albums")
            .fetch_one(&self.pool)
            .await
            .map_err(LibraryError::Db)?;

        Ok(LibraryStats {
            photo_count: row.0 as u64,
            video_count: row.1 as u64,
            album_count: album_count.0 as u64,
            total_file_size: row.2 as u64,
        })
    }
}

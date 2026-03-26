use crate::library::error::LibraryError;

use super::Database;

impl Database {
    /// Insert a file into the upload queue as pending.
    pub async fn insert_upload_pending(
        &self,
        file_path: &str,
        created_at: i64,
    ) -> Result<(), LibraryError> {
        sqlx::query(
            "INSERT OR IGNORE INTO upload_queue (file_path, status, created_at) VALUES (?, 0, ?)",
        )
        .bind(file_path)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Store the computed SHA-1 hash for a queued file.
    pub async fn set_upload_hash(
        &self,
        file_path: &str,
        sha1_hash: &str,
    ) -> Result<(), LibraryError> {
        sqlx::query("UPDATE upload_queue SET sha1_hash = ? WHERE file_path = ?")
            .bind(sha1_hash)
            .bind(file_path)
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Update the status of a queued upload.
    pub async fn set_upload_status(
        &self,
        file_path: &str,
        status: i64,
        error_msg: Option<&str>,
    ) -> Result<(), LibraryError> {
        sqlx::query("UPDATE upload_queue SET status = ?, error_msg = ? WHERE file_path = ?")
            .bind(status)
            .bind(error_msg)
            .bind(file_path)
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Remove all completed/duplicate uploads from the queue.
    pub async fn clear_completed_uploads(&self) -> Result<(), LibraryError> {
        sqlx::query("DELETE FROM upload_queue WHERE status IN (1, 3)")
            .execute(&self.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }
}

// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use super::model::MaintenanceTask;
use crate::library::db::Database;
use crate::library::error::LibraryError;

/// `library_maintenance` persistence: which one-shot tasks have run.
#[derive(Clone)]
pub struct MaintenanceRepository {
    db: Database,
}

impl MaintenanceRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Return `true` if `task` has been recorded as completed.
    pub async fn is_completed(&self, task: MaintenanceTask) -> Result<bool, LibraryError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM library_maintenance WHERE task = ?")
                .bind(task.key())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;
        Ok(row.is_some())
    }

    /// Record `task` as completed at `completed_at` (Unix seconds).
    /// A second call keeps the first timestamp.
    pub async fn mark_completed(
        &self,
        task: MaintenanceTask,
        completed_at: i64,
    ) -> Result<(), LibraryError> {
        sqlx::query("INSERT OR IGNORE INTO library_maintenance (task, completed_at) VALUES (?, ?)")
            .bind(task.key())
            .bind(completed_at)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::open_test_db;

    #[tokio::test]
    async fn mark_completed_is_persistent_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let repo = MaintenanceRepository::new(open_test_db(dir.path()).await);
        let task = MaintenanceTask::TakenAtOffsetBackfill;

        assert!(!repo.is_completed(task).await.unwrap());
        repo.mark_completed(task, 100).await.unwrap();
        repo.mark_completed(task, 200).await.unwrap();
        assert!(repo.is_completed(task).await.unwrap());
    }
}

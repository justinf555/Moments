use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

use super::model::EditState;

/// Editing persistence layer.
///
/// Encapsulates all edit-state SQL queries (JSON storage in the `edits` table).
#[derive(Clone)]
pub struct EditingRepository {
    db: Database,
}

impl EditingRepository {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Get the current edit state for a media item.
    pub async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError> {
        let id_str = id.as_str();
        let row: Option<(String,)> =
            sqlx::query_as("SELECT edit_json FROM edits WHERE media_id = ?")
                .bind(id_str)
                .fetch_optional(&self.db.pool)
                .await
                .map_err(LibraryError::Db)?;

        match row {
            Some((json,)) => {
                let state: EditState = serde_json::from_str(&json)
                    .map_err(|e| LibraryError::Runtime(e.to_string()))?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }

    /// Save or update the edit state for a media item.
    pub async fn upsert_edit_state(
        &self,
        id: &MediaId,
        state: &EditState,
    ) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        let json =
            serde_json::to_string(state).map_err(|e| LibraryError::Runtime(e.to_string()))?;
        let now = chrono::Utc::now().timestamp();

        sqlx::query(
            "INSERT INTO edits (media_id, edit_json, updated_at)
             VALUES (?, ?, ?)
             ON CONFLICT(media_id) DO UPDATE SET
                 edit_json = excluded.edit_json,
                 updated_at = excluded.updated_at,
                 rendered_at = NULL",
        )
        .bind(id_str)
        .bind(&json)
        .bind(now)
        .execute(&self.db.pool)
        .await
        .map_err(LibraryError::Db)?;

        Ok(())
    }

    /// Delete the edit state for a media item (revert to original).
    pub async fn delete_edit_state(&self, id: &MediaId) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        sqlx::query("DELETE FROM edits WHERE media_id = ?")
            .bind(id_str)
            .execute(&self.db.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Mark the edit as rendered (uploaded to server).
    pub async fn mark_edit_rendered(&self, id: &MediaId) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE edits SET rendered_at = ? WHERE media_id = ?")
            .bind(now)
            .bind(id_str)
            .execute(&self.db.pool)
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }

    /// Check whether an asset has edits that haven't been rendered yet.
    pub async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError> {
        let id_str = id.as_str();
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM edits WHERE media_id = ? AND (rendered_at IS NULL OR updated_at > rendered_at)",
        )
        .bind(id_str)
        .fetch_optional(&self.db.pool)
        .await
        .map_err(LibraryError::Db)?;

        Ok(row.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, test_record};
    use crate::library::media::repository::MediaRepository;
    use crate::library::media::MediaId;

    async fn test_repo(dir: &std::path::Path) -> (EditingRepository, MediaRepository) {
        let db = open_test_db(dir).await;
        let repo = EditingRepository::new(db.clone());
        let media = MediaRepository::new(db);
        (repo, media)
    }

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, _media) = test_repo(dir.path()).await;
        let id = MediaId::new("abc123".to_string());
        let result = repo.get_edit_state(&id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn upsert_and_get_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, media) = test_repo(dir.path()).await;
        let id = MediaId::new("abc123".to_string());
        media.insert(&test_record(id.clone())).await.unwrap();

        let mut state = EditState::default();
        state.exposure.brightness = 0.5;
        state.color.saturation = -0.3;

        repo.upsert_edit_state(&id, &state).await.unwrap();
        let loaded = repo.get_edit_state(&id).await.unwrap().unwrap();
        assert_eq!(loaded, state);
    }

    #[tokio::test]
    async fn upsert_overwrites_and_clears_rendered() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, media) = test_repo(dir.path()).await;
        let id = MediaId::new("abc123".to_string());
        media.insert(&test_record(id.clone())).await.unwrap();

        let state = EditState::default();
        repo.upsert_edit_state(&id, &state).await.unwrap();
        repo.mark_edit_rendered(&id).await.unwrap();

        // Verify rendered
        assert!(!repo.has_pending_edits(&id).await.unwrap());

        // Update should clear rendered_at
        let mut updated = EditState::default();
        updated.exposure.contrast = 0.2;
        repo.upsert_edit_state(&id, &updated).await.unwrap();
        assert!(repo.has_pending_edits(&id).await.unwrap());
    }

    #[tokio::test]
    async fn delete_removes_edit_state() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, media) = test_repo(dir.path()).await;
        let id = MediaId::new("abc123".to_string());
        media.insert(&test_record(id.clone())).await.unwrap();

        let state = EditState::default();
        repo.upsert_edit_state(&id, &state).await.unwrap();
        assert!(repo.get_edit_state(&id).await.unwrap().is_some());

        repo.delete_edit_state(&id).await.unwrap();
        assert!(repo.get_edit_state(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn has_pending_edits_returns_false_when_no_edits() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, _media) = test_repo(dir.path()).await;
        let id = MediaId::new("abc123".to_string());
        assert!(!repo.has_pending_edits(&id).await.unwrap());
    }
}

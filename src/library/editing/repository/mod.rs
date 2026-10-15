// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

use super::model::EditState;

mod sync;

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
                .fetch_optional(self.db.pool())
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
        .execute(self.db.pool())
        .await
        .map_err(LibraryError::Db)?;

        Ok(())
    }

    /// Delete the edit state for a media item (revert to original).
    pub async fn delete_edit_state(&self, id: &MediaId) -> Result<(), LibraryError> {
        let id_str = id.as_str();
        sqlx::query("DELETE FROM edits WHERE media_id = ?")
            .bind(id_str)
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
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
}

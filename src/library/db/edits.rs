//! Thin forwarding layer for edit operations on `Database`.
//!
//! All SQL lives in `EditingRepository` (`library/editing/repository.rs`).
//! This module exists so that code holding a `Database` can still call
//! edit methods directly. It will be removed when all features are
//! converted to repositories.

use crate::library::editing::model::EditState;
use crate::library::editing::repository::EditingRepository;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

use super::Database;

impl Database {
    pub async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError> {
        EditingRepository::new(self.clone())
            .get_edit_state(id)
            .await
    }

    pub async fn upsert_edit_state(
        &self,
        id: &MediaId,
        state: &EditState,
    ) -> Result<(), LibraryError> {
        EditingRepository::new(self.clone())
            .upsert_edit_state(id, state)
            .await
    }

    pub async fn delete_edit_state(&self, id: &MediaId) -> Result<(), LibraryError> {
        EditingRepository::new(self.clone())
            .delete_edit_state(id)
            .await
    }

    pub async fn mark_edit_rendered(&self, id: &MediaId) -> Result<(), LibraryError> {
        EditingRepository::new(self.clone())
            .mark_edit_rendered(id)
            .await
    }

    pub async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError> {
        EditingRepository::new(self.clone())
            .has_pending_edits(id)
            .await
    }
}

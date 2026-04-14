use async_trait::async_trait;

use super::model::EditState;
use super::repository::EditingRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

/// Feature trait for non-destructive photo editing.
///
/// Edit operations are stored as JSON and applied on the fly during
/// display. For the Immich backend, `render_and_save` uploads the
/// rendered result as an edited version. For the local backend, edits
/// are applied during viewing and thumbnail generation.
#[async_trait]
pub trait LibraryEditing: Send + Sync {
    /// Get the current edit state for a media item.
    /// Returns `None` if no edits have been applied.
    async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError>;

    /// Save the current edit state for a media item.
    /// Overwrites any existing state.
    async fn save_edit_state(&self, id: &MediaId, state: &EditState) -> Result<(), LibraryError>;

    /// Remove all edits for a media item (revert to original).
    async fn revert_edits(&self, id: &MediaId) -> Result<(), LibraryError>;

    /// Render the current edit state to a full-resolution image and persist it.
    /// For Immich: uploads as edited version. For local: no-op (edits applied on the fly).
    async fn render_and_save(&self, id: &MediaId) -> Result<(), LibraryError>;

    /// Check whether an asset has unsaved/unrendered edits.
    async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError>;
}

/// Local-first editing service.
///
/// Implements [`LibraryEditing`] by delegating to [`EditingRepository`].
#[derive(Clone)]
pub struct EditingService {
    pub(crate) repo: EditingRepository,
}

impl EditingService {
    pub fn new(db: Database) -> Self {
        Self {
            repo: EditingRepository::new(db),
        }
    }
}

#[async_trait]
impl LibraryEditing for EditingService {
    async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError> {
        self.repo.get_edit_state(id).await
    }

    async fn save_edit_state(&self, id: &MediaId, state: &EditState) -> Result<(), LibraryError> {
        self.repo.upsert_edit_state(id, state).await
    }

    async fn revert_edits(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.delete_edit_state(id).await
    }

    async fn render_and_save(&self, _id: &MediaId) -> Result<(), LibraryError> {
        // Local backend applies edits on the fly during viewing.
        Ok(())
    }

    async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.repo.has_pending_edits(id).await
    }
}

use super::model::EditState;
use super::repository::EditingRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

/// Non-destructive photo editing service.
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

    pub async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError> {
        self.repo.get_edit_state(id).await
    }

    pub async fn save_edit_state(
        &self,
        id: &MediaId,
        state: &EditState,
    ) -> Result<(), LibraryError> {
        self.repo.upsert_edit_state(id, state).await
    }

    pub async fn revert_edits(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.delete_edit_state(id).await
    }

    pub async fn render_and_save(&self, _id: &MediaId) -> Result<(), LibraryError> {
        // Local backend applies edits on the fly during viewing.
        Ok(())
    }

    pub async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.repo.has_pending_edits(id).await
    }
}

use std::sync::Arc;

use super::model::EditState;
use super::repository::EditingRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;

/// Non-destructive photo editing service.
#[derive(Clone)]
pub struct EditingService {
    repo: EditingRepository,
    recorder: Arc<dyn MutationRecorder>,
}

impl EditingService {
    pub fn new(db: Database, recorder: Arc<dyn MutationRecorder>) -> Self {
        Self {
            repo: EditingRepository::new(db),
            recorder,
        }
    }

    pub async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError> {
        self.repo.get_edit_state(id).await
    }

    /// Persist the edit state and signal an outbox-bound mutation.
    ///
    /// The mutation is payload-free (`AssetEditsApplied { id }`) — the push
    /// handler reads the latest `EditState` at drain time, so multiple
    /// rapid saves naturally coalesce into one wire call. If the saved
    /// state is the identity, we record a clear instead so any prior
    /// server-side edit is removed.
    pub async fn save_edit_state(
        &self,
        id: &MediaId,
        state: &EditState,
    ) -> Result<(), LibraryError> {
        self.repo.upsert_edit_state(id, state).await?;
        let mutation = if state.is_identity() {
            Mutation::AssetEditsCleared { id: id.clone() }
        } else {
            Mutation::AssetEditsApplied { id: id.clone() }
        };
        self.recorder.record(&mutation).await
    }

    pub async fn revert_edits(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.delete_edit_state(id).await?;
        self.recorder
            .record(&Mutation::AssetEditsCleared { id: id.clone() })
            .await
    }

    pub async fn render_and_save(&self, _id: &MediaId) -> Result<(), LibraryError> {
        // Local backend applies edits on the fly during viewing.
        Ok(())
    }

    pub async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.repo.has_pending_edits(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::{open_test_db, test_record};
    use crate::library::media::repository::MediaRepository;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Captures every mutation passed to `record`. Used to assert the
    /// service emits the right outbox-bound signal on save/revert.
    #[derive(Default)]
    struct CapturingRecorder {
        recorded: Mutex<Vec<Mutation>>,
    }

    impl CapturingRecorder {
        fn snapshot(&self) -> Vec<Mutation> {
            self.recorded.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MutationRecorder for CapturingRecorder {
        async fn record(&self, mutation: &Mutation) -> Result<(), LibraryError> {
            self.recorded.lock().unwrap().push(mutation.clone());
            Ok(())
        }
    }

    async fn fixture() -> (
        tempfile::TempDir,
        EditingService,
        Arc<CapturingRecorder>,
        MediaRepository,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let recorder = Arc::new(CapturingRecorder::default());
        let svc = EditingService::new(
            db.clone(),
            Arc::clone(&recorder) as Arc<dyn MutationRecorder>,
        );
        let media = MediaRepository::new(db);
        (dir, svc, recorder, media)
    }

    #[tokio::test]
    async fn save_with_real_edit_records_applied() {
        let (_dir, svc, recorder, media) = fixture().await;
        let id = MediaId::new("photo-1".into());
        media.insert(&test_record(id.clone())).await.unwrap();

        let mut state = EditState::default();
        state.exposure.brightness = 0.5;

        svc.save_edit_state(&id, &state).await.unwrap();

        let recorded = recorder.snapshot();
        assert_eq!(recorded.len(), 1);
        assert!(matches!(
            recorded[0],
            Mutation::AssetEditsApplied { ref id } if id.as_str() == "photo-1"
        ));
    }

    #[tokio::test]
    async fn save_with_identity_state_records_cleared() {
        // Saving an identity state — e.g. user undid all sliders without
        // hitting Revert — should remove any prior server-side edit.
        let (_dir, svc, recorder, media) = fixture().await;
        let id = MediaId::new("photo-2".into());
        media.insert(&test_record(id.clone())).await.unwrap();

        svc.save_edit_state(&id, &EditState::default())
            .await
            .unwrap();

        let recorded = recorder.snapshot();
        assert_eq!(recorded.len(), 1);
        assert!(matches!(
            recorded[0],
            Mutation::AssetEditsCleared { ref id } if id.as_str() == "photo-2"
        ));
    }

    #[tokio::test]
    async fn revert_records_cleared() {
        let (_dir, svc, recorder, media) = fixture().await;
        let id = MediaId::new("photo-3".into());
        media.insert(&test_record(id.clone())).await.unwrap();

        let mut state = EditState::default();
        state.exposure.brightness = 0.5;
        svc.save_edit_state(&id, &state).await.unwrap();
        svc.revert_edits(&id).await.unwrap();

        let recorded = recorder.snapshot();
        assert_eq!(recorded.len(), 2);
        assert!(matches!(recorded[0], Mutation::AssetEditsApplied { .. }));
        assert!(matches!(
            recorded[1],
            Mutation::AssetEditsCleared { ref id } if id.as_str() == "photo-3"
        ));
    }
}

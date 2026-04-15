use std::sync::Arc;

use tracing::warn;

use super::model::{Person, PersonId};
use super::repository::FacesRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;

/// Face/people management service.
#[derive(Clone)]
pub struct FacesService {
    repo: FacesRepository,
    thumbnails_dir: Option<std::path::PathBuf>,
    recorder: Arc<dyn MutationRecorder>,
}

impl FacesService {
    /// Create a faces service backed by a database.
    ///
    /// Pass `thumbnails_dir` for backends that store person thumbnails
    /// (Immich). Pass `None` for backends without face detection (local).
    pub fn new(
        db: Database,
        thumbnails_dir: Option<std::path::PathBuf>,
        recorder: Arc<dyn MutationRecorder>,
    ) -> Self {
        Self {
            repo: FacesRepository::new(db),
            thumbnails_dir,
            recorder,
        }
    }

    // ── Sync upserts (pull from server, no outbox recording) ───────

    /// Insert or replace a person from the sync stream.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_person(
        &self,
        id: &str,
        name: &str,
        birth_date: Option<&str>,
        is_hidden: bool,
        is_favorite: bool,
        color: Option<&str>,
        face_asset_id: Option<&str>,
        external_id: Option<&str>,
    ) -> Result<(), LibraryError> {
        self.repo
            .upsert_person(
                id,
                name,
                birth_date,
                is_hidden,
                is_favorite,
                color,
                face_asset_id,
                external_id,
            )
            .await
    }

    /// Insert or replace an asset face from the sync stream.
    pub(crate) async fn upsert_asset_face(
        &self,
        face: &super::repository::AssetFaceRow,
    ) -> Result<(), LibraryError> {
        self.repo.upsert_asset_face(face).await
    }

    /// Delete a person by ID (sync stream delete).
    pub async fn delete_person_by_id(&self, id: &str) -> Result<(), LibraryError> {
        self.repo.delete_person(id).await
    }

    /// Delete an asset face by ID (sync stream delete).
    pub async fn delete_asset_face(&self, id: &str) -> Result<(), LibraryError> {
        self.repo.delete_asset_face(id).await
    }

    /// Update the denormalized face count for a person.
    pub async fn update_face_count(&self, person_id: &str) -> Result<(), LibraryError> {
        self.repo.update_face_count(person_id).await
    }

    /// Clear all people (for reset sync).
    pub async fn clear_people(&self) -> Result<(), LibraryError> {
        self.repo.clear_people().await
    }

    /// Clear all asset faces (for reset sync).
    pub async fn clear_asset_faces(&self) -> Result<(), LibraryError> {
        self.repo.clear_asset_faces().await
    }

    // ── Query methods ───────────────────────────────────────────────

    pub async fn list_people(
        &self,
        include_hidden: bool,
        include_unnamed: bool,
    ) -> Result<Vec<Person>, LibraryError> {
        self.repo.list_people(include_hidden, include_unnamed).await
    }

    pub async fn list_media_for_person(
        &self,
        person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError> {
        let ids = self.repo.list_media_for_person(person_id.as_str()).await?;
        Ok(ids.into_iter().map(MediaId::new).collect())
    }

    pub async fn rename_person(
        &self,
        person_id: &PersonId,
        name: &str,
    ) -> Result<(), LibraryError> {
        self.repo.rename_person(person_id.as_str(), name).await?;
        if let Err(e) = self
            .recorder
            .record(&Mutation::PersonRenamed {
                id: person_id.clone(),
                name: name.to_string(),
            })
            .await
        {
            warn!(error = %e, "failed to record PersonRenamed mutation");
        }
        Ok(())
    }

    pub async fn set_person_hidden(
        &self,
        person_id: &PersonId,
        hidden: bool,
    ) -> Result<(), LibraryError> {
        self.repo
            .set_person_hidden(person_id.as_str(), hidden)
            .await?;
        if let Err(e) = self
            .recorder
            .record(&Mutation::PersonHidden {
                id: person_id.clone(),
                hidden,
            })
            .await
        {
            warn!(error = %e, "failed to record PersonHidden mutation");
        }
        Ok(())
    }

    pub async fn merge_people(
        &self,
        _target: &PersonId,
        _sources: &[PersonId],
    ) -> Result<(), LibraryError> {
        // TODO: implement local merge (#185)
        Ok(())
    }

    pub fn person_thumbnail_path(&self, person_id: &PersonId) -> Option<std::path::PathBuf> {
        let dir = self.thumbnails_dir.as_ref()?;
        let path = dir
            .join("people")
            .join(format!("{}.jpg", person_id.as_str()));
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }
}

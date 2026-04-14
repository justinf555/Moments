use super::model::{Person, PersonId};
use super::repository::FacesRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

/// Face/people management service.
#[derive(Clone)]
pub struct FacesService {
    pub(crate) repo: FacesRepository,
    thumbnails_dir: Option<std::path::PathBuf>,
}

impl FacesService {
    /// Create a faces service backed by a database.
    ///
    /// Pass `thumbnails_dir` for backends that store person thumbnails
    /// (Immich). Pass `None` for backends without face detection (local).
    pub fn new(db: Database, thumbnails_dir: Option<std::path::PathBuf>) -> Self {
        Self {
            repo: FacesRepository::new(db),
            thumbnails_dir,
        }
    }

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
        self.repo.rename_person(person_id.as_str(), name).await
    }

    pub async fn set_person_hidden(
        &self,
        person_id: &PersonId,
        hidden: bool,
    ) -> Result<(), LibraryError> {
        self.repo
            .set_person_hidden(person_id.as_str(), hidden)
            .await
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

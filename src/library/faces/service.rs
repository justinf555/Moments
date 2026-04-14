use async_trait::async_trait;

use super::model::{Person, PersonId};
use super::repository::FacesRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

/// Feature trait for face/people management.
///
/// Implemented by every backend. The Immich backend reads from locally
/// synced `people` / `asset_faces` tables. The local backend returns
/// empty results until a local face detection pipeline is added.
#[async_trait]
pub trait LibraryFaces: Send + Sync {
    /// List all people, ordered by face count descending.
    ///
    /// If `include_hidden` is false, hidden people are excluded.
    /// If `include_unnamed` is false, people with empty names are excluded.
    async fn list_people(
        &self,
        include_hidden: bool,
        include_unnamed: bool,
    ) -> Result<Vec<Person>, LibraryError>;

    /// Get media IDs for all assets containing a specific person.
    async fn list_media_for_person(
        &self,
        person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError>;

    /// Rename a person.
    async fn rename_person(&self, person_id: &PersonId, name: &str) -> Result<(), LibraryError>;

    /// Hide or unhide a person.
    async fn set_person_hidden(
        &self,
        person_id: &PersonId,
        hidden: bool,
    ) -> Result<(), LibraryError>;

    /// Merge source people into the target person.
    /// All faces from the source people are reassigned to the target,
    /// and the source people are deleted.
    async fn merge_people(
        &self,
        target: &PersonId,
        sources: &[PersonId],
    ) -> Result<(), LibraryError>;

    /// Return the filesystem path to a person's face thumbnail, if available.
    ///
    /// The Immich backend stores these at `{thumbnails_dir}/people/{id}.jpg`.
    /// The local backend returns `None`.
    fn person_thumbnail_path(&self, person_id: &PersonId) -> Option<std::path::PathBuf>;
}

/// Local-first faces service.
///
/// Implements [`LibraryFaces`] by delegating to [`FacesRepository`].
/// The `thumbnails_dir` is needed for `person_thumbnail_path`.
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
}

#[async_trait]
impl LibraryFaces for FacesService {
    async fn list_people(
        &self,
        include_hidden: bool,
        include_unnamed: bool,
    ) -> Result<Vec<Person>, LibraryError> {
        self.repo.list_people(include_hidden, include_unnamed).await
    }

    async fn list_media_for_person(
        &self,
        person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError> {
        let ids = self.repo.list_media_for_person(person_id.as_str()).await?;
        Ok(ids.into_iter().map(MediaId::new).collect())
    }

    async fn rename_person(&self, person_id: &PersonId, name: &str) -> Result<(), LibraryError> {
        self.repo.rename_person(person_id.as_str(), name).await
    }

    async fn set_person_hidden(
        &self,
        person_id: &PersonId,
        hidden: bool,
    ) -> Result<(), LibraryError> {
        self.repo
            .set_person_hidden(person_id.as_str(), hidden)
            .await
    }

    async fn merge_people(
        &self,
        _target: &PersonId,
        _sources: &[PersonId],
    ) -> Result<(), LibraryError> {
        // TODO: implement local merge (#185)
        Ok(())
    }

    fn person_thumbnail_path(&self, person_id: &PersonId) -> Option<std::path::PathBuf> {
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

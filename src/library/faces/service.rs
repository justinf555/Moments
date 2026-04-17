use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

use super::event::FacesEvent;
use super::model::{Person, PersonId};
use super::repository::FacesRepository;
use crate::event_emitter::EventEmitter;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;

/// Face/people management service.
///
/// Holds an [`EventEmitter<FacesEvent>`] to notify clients of state changes.
/// Each call to [`subscribe`] returns a fresh receiver; every emitted event
/// is delivered to every live subscriber.
///
/// [`subscribe`]: FacesService::subscribe
#[derive(Clone)]
pub struct FacesService {
    repo: FacesRepository,
    thumbnails_dir: Option<std::path::PathBuf>,
    recorder: Arc<dyn MutationRecorder>,
    events: EventEmitter<FacesEvent>,
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
            events: EventEmitter::new(),
        }
    }

    /// Register a new subscriber. Every emitted event is delivered to every
    /// live subscriber.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<FacesEvent> {
        self.events.subscribe()
    }

    /// Broadcast an event to every live subscriber.
    fn emit(&self, event: FacesEvent) {
        self.events.emit(event);
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
        // Check if person exists before upsert to distinguish add vs update.
        let existed = self.repo.get_person(id).await?.is_some();
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
            .await?;
        let person_id = PersonId::from_raw(id.to_string());
        if existed {
            self.emit(FacesEvent::PersonUpdated(person_id));
        } else {
            self.emit(FacesEvent::PersonAdded(person_id));
        }
        Ok(())
    }

    /// Insert or replace an asset face from the sync stream.
    ///
    /// Emits `PersonMediaChanged` for every person whose membership set
    /// changed. For a reassignment from A to B, two events fire — one for
    /// A (a media was removed) and one for B (a media was added). If the
    /// person_id is unchanged, no events fire.
    pub(crate) async fn upsert_asset_face(
        &self,
        face: &super::repository::AssetFaceRow,
    ) -> Result<(), LibraryError> {
        let prev_person_id = self.repo.get_asset_face_person_id(&face.id).await?;
        self.repo.upsert_asset_face(face).await?;
        let new_person_id = face.person_id.clone();
        if prev_person_id != new_person_id {
            if let Some(p) = prev_person_id {
                self.emit(FacesEvent::PersonMediaChanged(PersonId::from_raw(p)));
            }
            if let Some(p) = new_person_id {
                self.emit(FacesEvent::PersonMediaChanged(PersonId::from_raw(p)));
            }
        }
        Ok(())
    }

    /// Delete a person by ID (sync stream delete).
    pub async fn delete_person_by_id(&self, id: &str) -> Result<(), LibraryError> {
        self.repo.delete_person(id).await?;
        self.emit(FacesEvent::PersonRemoved(PersonId::from_raw(
            id.to_string(),
        )));
        Ok(())
    }

    /// Delete an asset face by ID (sync stream delete).
    ///
    /// Emits `PersonMediaChanged` for the deleted face's person, if any.
    pub async fn delete_asset_face(&self, id: &str) -> Result<(), LibraryError> {
        let deleted_person_id = self.repo.delete_asset_face(id).await?;
        if let Some(p) = deleted_person_id {
            self.emit(FacesEvent::PersonMediaChanged(PersonId::from_raw(p)));
        }
        Ok(())
    }

    /// Update the denormalized face count for a person.
    ///
    /// No `FacesEvent` is emitted — `face_count` is not exposed as a
    /// GObject property on `PersonItemObject`, so there is nothing to
    /// patch on the client. Emitting here would cause an O(faces) storm
    /// of no-op DB roundtrips during bulk sync.
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

    pub async fn list_people(&self) -> Result<Vec<Person>, LibraryError> {
        self.repo.list_people().await
    }

    pub async fn get_person(&self, person_id: &PersonId) -> Result<Option<Person>, LibraryError> {
        self.repo.get_person(person_id.as_str()).await
    }

    pub async fn list_media_for_person(
        &self,
        person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError> {
        let ids = self.repo.list_media_for_person(person_id.as_str()).await?;
        Ok(ids.into_iter().map(MediaId::new).collect())
    }

    /// Rename a person.
    ///
    /// No `FacesEvent` is emitted — `PeopleClientV2` patches the model
    /// directly in its success callback to avoid a redundant DB roundtrip.
    /// Callers that bypass the client must patch the UI themselves.
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

    /// Set a person's hidden state.
    ///
    /// No `FacesEvent` is emitted — `PeopleClientV2` patches the model
    /// directly in its success callback to avoid a redundant DB roundtrip.
    /// Callers that bypass the client must patch the UI themselves.
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

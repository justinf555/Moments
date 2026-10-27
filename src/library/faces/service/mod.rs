// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tokio::sync::mpsc;

use super::event::FacesEvent;
use super::model::{Person, PersonId};
use super::repository::FacesRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::event_emitter::EventEmitter;
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;

mod sync;

/// Face/people management service.
///
/// Holds an [`EventEmitter<FacesEvent>`] to notify clients of state changes.
/// Each call to [`subscribe`] returns a fresh receiver; every emitted event
/// is delivered to every live subscriber.
///
/// [`subscribe`]: FacesService::subscribe
#[derive(Clone)]
pub struct FacesService {
    db: Database,
    repo: FacesRepository,
    thumbnails_dir: std::path::PathBuf,
    recorder: Arc<dyn MutationRecorder>,
    events: EventEmitter<FacesEvent>,
}

impl FacesService {
    /// Create a faces service backed by a database.
    ///
    /// `thumbnails_dir` is the bundle's thumbnails root — person face
    /// thumbnails are stored under `{thumbnails_dir}/people/{id}.jpg`
    /// by the sync handler and read back by [`Self::person_thumbnail_path`].
    /// Local backends pass the same dir even though they never write into
    /// it; that keeps the contract uniform across backends.
    pub fn new(
        db: Database,
        thumbnails_dir: std::path::PathBuf,
        recorder: Arc<dyn MutationRecorder>,
    ) -> Self {
        Self {
            repo: FacesRepository::new(db.clone()),
            db,
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

    // ── Query methods ───────────────────────────────────────────────

    pub async fn list_people(&self) -> Result<Vec<Person>, LibraryError> {
        self.repo.list_people().await
    }

    pub async fn get_person(&self, person_id: &PersonId) -> Result<Option<Person>, LibraryError> {
        self.repo.get_person(person_id.as_str()).await
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
        let mut tx = self.db.begin().await?;
        self.repo
            .rename_person_in_tx(&mut tx, person_id.as_str(), name)
            .await?;
        self.recorder
            .record(
                &mut tx,
                &Mutation::PersonRenamed {
                    id: person_id.clone(),
                    name: name.to_string(),
                },
            )
            .await?;
        tx.commit().await?;
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
        let mut tx = self.db.begin().await?;
        self.repo
            .set_person_hidden_in_tx(&mut tx, person_id.as_str(), hidden)
            .await?;
        self.recorder
            .record(
                &mut tx,
                &Mutation::PersonHidden {
                    id: person_id.clone(),
                    hidden,
                },
            )
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Return the on-disk path of a person's face thumbnail, if it has
    /// been downloaded. Returns `None` when the file is missing — UI
    /// callers fall back to rendering the person's initials.
    pub fn person_thumbnail_path(&self, person_id: &PersonId) -> Option<std::path::PathBuf> {
        let path = self
            .thumbnails_dir
            .join("people")
            .join(format!("{}.jpg", person_id.as_str()));
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::open_test_db;
    use crate::sync::outbox::NoOpRecorder;

    pub(super) async fn make_service(
        thumbnails_dir: std::path::PathBuf,
    ) -> (tempfile::TempDir, FacesService) {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let svc = FacesService::new(db, thumbnails_dir, Arc::new(NoOpRecorder));
        (dir, svc)
    }

    /// Sync handler writes to `{thumbnails_dir}/people/{id}.jpg`; the
    /// service must read from the same location (#611).
    #[tokio::test]
    async fn person_thumbnail_path_returns_file_when_present() {
        let thumb_root = tempfile::tempdir().unwrap();
        let people_dir = thumb_root.path().join("people");
        std::fs::create_dir_all(&people_dir).unwrap();
        let person_id = PersonId::new("person-uuid".to_string());
        let thumb_file = people_dir.join("person-uuid.jpg");
        std::fs::write(&thumb_file, b"jpeg bytes").unwrap();

        let (_dir, svc) = make_service(thumb_root.path().to_path_buf()).await;
        let result = svc.person_thumbnail_path(&person_id);
        assert_eq!(result, Some(thumb_file));
    }

    /// No file on disk → None, so the UI falls back to initials.
    #[tokio::test]
    async fn person_thumbnail_path_returns_none_when_absent() {
        let thumb_root = tempfile::tempdir().unwrap();
        let person_id = PersonId::new("never-downloaded".to_string());

        let (_dir, svc) = make_service(thumb_root.path().to_path_buf()).await;
        assert!(svc.person_thumbnail_path(&person_id).is_none());
    }

    // ── Transactional outbox (#655) ─────────────────────────────────

    use crate::library::recorder::tests::{outbox_count, FailingRecorder};

    #[tokio::test]
    async fn recorder_failure_rolls_back_person_writes() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let svc = FacesService::new(
            db.clone(),
            dir.path().join("thumbnails"),
            Arc::new(FailingRecorder::always(db.clone())),
        );
        svc.upsert_person("p1", "Alice", None, false, false, None, None, None)
            .await
            .unwrap();
        let id = PersonId::new("p1".to_string());

        assert!(svc.rename_person(&id, "Mallory").await.is_err());
        assert!(svc.set_person_hidden(&id, true).await.is_err());

        let person = svc.get_person(&id).await.unwrap().unwrap();
        assert_eq!(person.name, "Alice", "rename must roll back");
        assert!(!person.is_hidden, "hide must roll back");
        assert_eq!(outbox_count(&db).await, 0);
    }
}

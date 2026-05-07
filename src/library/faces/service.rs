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

    /// Sync-only: bump `last_seen_at` for one person row. See issue
    /// #628 — the heartbeat that the reset-cycle orphan sweep
    /// compares against.
    pub async fn bump_person_last_seen_at(&self, id: &str, now: i64) -> Result<(), LibraryError> {
        self.repo.bump_person_last_seen_at(id, now).await
    }

    /// Sync-only: bump `last_seen_at` for one asset_face row. See
    /// issue #628.
    pub async fn bump_asset_face_last_seen_at(
        &self,
        id: &str,
        now: i64,
    ) -> Result<(), LibraryError> {
        self.repo.bump_asset_face_last_seen_at(id, now).await
    }

    /// Sync-only: delete people whose heartbeat lags `checkpoint`.
    /// Returns the deleted person ids. Emits
    /// [`FacesEvent::PersonRemoved`] for each so client `ListStore`s
    /// drop the rows without waiting for a UI refresh. See issue #628.
    pub async fn delete_people_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<Vec<String>, LibraryError> {
        let removed_ids = self
            .repo
            .delete_people_with_stale_heartbeat(checkpoint)
            .await?;
        for id in &removed_ids {
            self.emit(FacesEvent::PersonRemoved(PersonId::from_raw(id.clone())));
        }
        Ok(removed_ids)
    }

    /// Sync-only: delete asset_face rows whose heartbeat lags
    /// `checkpoint`. Returns the count of deleted rows.
    ///
    /// Recomputes `face_count` on every surviving person whose stale
    /// faces were swept — without this the denormalised count drifts
    /// until the next sync cycle re-emits the person. See issue #628.
    pub async fn delete_asset_faces_with_stale_heartbeat(
        &self,
        checkpoint: i64,
    ) -> Result<u64, LibraryError> {
        let affected_persons = self.repo.persons_with_stale_faces(checkpoint).await?;
        let removed = self
            .repo
            .delete_asset_faces_with_stale_heartbeat(checkpoint)
            .await?;
        for person_id in &affected_persons {
            self.repo.update_face_count(person_id).await?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::open_test_db;
    use crate::sync::outbox::NoOpRecorder;

    async fn make_service(thumbnails_dir: std::path::PathBuf) -> (tempfile::TempDir, FacesService) {
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
        let person_id = PersonId::from_raw("person-uuid".to_string());
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
        let person_id = PersonId::from_raw("never-downloaded".to_string());

        let (_dir, svc) = make_service(thumb_root.path().to_path_buf()).await;
        assert!(svc.person_thumbnail_path(&person_id).is_none());
    }

    /// Issue #628: orphan sweep emits `PersonRemoved` per deleted
    /// person so client `ListStore`s drop them in real time.
    #[tokio::test]
    async fn delete_people_with_stale_heartbeat_emits_person_removed_per_id() {
        let thumb_root = tempfile::tempdir().unwrap();
        let (_dir, svc) = make_service(thumb_root.path().to_path_buf()).await;

        svc.upsert_person("p1", "Stale", None, false, false, None, None, None)
            .await
            .unwrap();
        svc.upsert_person("p2", "Fresh", None, false, false, None, None, None)
            .await
            .unwrap();
        svc.repo.bump_person_last_seen_at("p1", 100).await.unwrap();
        svc.repo.bump_person_last_seen_at("p2", 300).await.unwrap();

        // Subscribe AFTER setup so the upsert events don't pollute the channel.
        let mut rx = svc.subscribe();

        let removed = svc.delete_people_with_stale_heartbeat(200).await.unwrap();
        assert_eq!(removed, vec!["p1".to_string()]);

        let event = rx.try_recv().expect("expected one event");
        match event {
            FacesEvent::PersonRemoved(id) => assert_eq!(id.as_str(), "p1"),
            other => panic!("expected PersonRemoved; got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "fresh person wasn't swept");
    }

    /// Issue #628: when stale faces are swept, every surviving person
    /// who lost faces gets their denormalised `face_count` recomputed.
    /// Without this the count drifts until the next sync cycle re-emits
    /// the person.
    #[tokio::test]
    async fn delete_asset_faces_with_stale_heartbeat_recomputes_face_count() {
        use crate::library::db::test_helpers::test_record;
        use crate::library::faces::repository::AssetFaceRow;
        use crate::library::media::repository::MediaRepository;
        use crate::library::media::MediaId;

        let thumb_root = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let media = MediaRepository::new(db.clone());
        let svc = FacesService::new(db.clone(), thumb_root.path().to_path_buf(), Arc::new(NoOpRecorder));

        media
            .insert(&test_record(MediaId::new("m1".to_string())))
            .await
            .unwrap();
        svc.upsert_person("p1", "Survivor", None, false, false, None, None, None)
            .await
            .unwrap();
        svc.repo.bump_person_last_seen_at("p1", 1_000).await.unwrap();

        // Two faces attached to the same surviving person — one stale,
        // one fresh.
        for (id, beat) in [("stale-face", 100), ("fresh-face", 1_000)] {
            let row = AssetFaceRow {
                id: id.to_string(),
                asset_id: "m1".to_string(),
                person_id: Some("p1".to_string()),
                image_width: 100,
                image_height: 100,
                bbox_x1: 0,
                bbox_y1: 0,
                bbox_x2: 50,
                bbox_y2: 50,
                source_type: "MachineLearning".to_string(),
            };
            svc.repo.upsert_asset_face(&row).await.unwrap();
            svc.repo.bump_asset_face_last_seen_at(id, beat).await.unwrap();
        }
        // Pin face_count to a wrong value so we can prove the recompute fired.
        sqlx::query("UPDATE people SET face_count = 99 WHERE id = 'p1'")
            .execute(db.pool())
            .await
            .unwrap();

        let removed = svc
            .delete_asset_faces_with_stale_heartbeat(200)
            .await
            .unwrap();
        assert_eq!(removed, 1);

        let count: (i64,) = sqlx::query_as("SELECT face_count FROM people WHERE id = 'p1'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            count.0, 1,
            "face_count must be recomputed to 1 (only fresh-face survives)"
        );
    }
}

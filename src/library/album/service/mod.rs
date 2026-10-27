// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::event::AlbumEvent;
use super::model::{Album, AlbumId};
use super::repository::AlbumRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::event_emitter::EventEmitter;
use crate::library::media::MediaId;
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;

mod sync;

/// Album management service.
///
/// Holds an [`EventEmitter<AlbumEvent>`] to notify clients of state changes.
/// Each call to [`subscribe`] returns a fresh receiver; every emitted event
/// is delivered to every live subscriber.
///
/// [`subscribe`]: AlbumService::subscribe
#[derive(Clone)]
pub struct AlbumService {
    db: Database,
    repo: AlbumRepository,
    recorder: Arc<dyn MutationRecorder>,
    events: EventEmitter<AlbumEvent>,
}

impl AlbumService {
    pub fn new(db: Database, recorder: Arc<dyn MutationRecorder>) -> Self {
        Self {
            repo: AlbumRepository::new(db.clone()),
            db,
            recorder,
            events: EventEmitter::new(),
        }
    }

    /// Register a new subscriber. Every emitted event is delivered to every
    /// live subscriber.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<AlbumEvent> {
        self.events.subscribe()
    }

    /// Broadcast an event to every live subscriber.
    fn emit(&self, event: AlbumEvent) {
        self.events.emit(event);
    }

    /// Emit `AlbumMediaChanged` for sync handlers that mutate the
    /// `album_media` table directly via the database (bypassing the
    /// service-level `add_to_album` / `remove_from_album` paths to avoid
    /// recording outbox mutations on pull).
    ///
    /// Intentionally does **not** emit `AlbumUpdated`. The service paths
    /// emit both because UI-driven membership changes need the album-row
    /// metadata refresh (item count, cover) immediately. For sync-pull
    /// callers the album-row metadata typically arrives in the same sync
    /// stream via the `AlbumV1` handler, which goes through `upsert_album`
    /// and emits `AlbumAdded`/`AlbumUpdated` itself — so the row refresh
    /// happens through that path, not this one.
    pub fn emit_album_media_changed(&self, album_id: &AlbumId) {
        self.emit(AlbumEvent::AlbumMediaChanged(album_id.clone()));
    }

    // ── Query methods ───────────────────────────────────────────────

    pub async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        self.repo.list().await
    }

    pub async fn get_album(&self, id: &AlbumId) -> Result<Option<Album>, LibraryError> {
        self.repo.get(id).await
    }

    pub async fn create_album(&self, name: &str) -> Result<AlbumId, LibraryError> {
        let mut tx = self.db.begin().await?;
        let id = self.repo.create_in_tx(&mut tx, name).await?;
        self.recorder
            .record(
                &mut tx,
                &Mutation::AlbumCreated {
                    id: id.clone(),
                    name: name.to_string(),
                },
            )
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn set_pinned(&self, id: &AlbumId, pinned: bool) -> Result<(), LibraryError> {
        self.repo.set_pinned(id, pinned).await
    }

    pub async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        let mut tx = self.db.begin().await?;
        self.repo.rename_in_tx(&mut tx, id, name).await?;
        self.recorder
            .record(
                &mut tx,
                &Mutation::AlbumRenamed {
                    id: id.clone(),
                    name: name.to_string(),
                },
            )
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError> {
        let mut tx = self.db.begin().await?;
        // Read the external_id inside the transaction, before the row
        // goes: the push handler needs it to delete the server album.
        let external_id = self.repo.external_id_in_tx(&mut tx, id).await?;
        self.repo.delete_in_tx(&mut tx, id).await?;
        self.recorder
            .record(
                &mut tx,
                &Mutation::AlbumDeleted {
                    id: id.clone(),
                    external_id,
                },
            )
            .await?;
        tx.commit().await?;
        self.emit(AlbumEvent::AlbumRemoved(id.clone()));
        Ok(())
    }

    pub async fn add_to_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        let mut tx = self.db.begin().await?;
        self.repo
            .add_media_in_tx(&mut tx, album_id, media_ids)
            .await?;
        self.recorder
            .record(
                &mut tx,
                &Mutation::AlbumMediaAdded {
                    album_id: album_id.clone(),
                    media_ids: media_ids.to_vec(),
                },
            )
            .await?;
        tx.commit().await?;
        // Emit both for now: AlbumUpdated preserves the existing
        // AlbumClientV2 refresh path; AlbumMediaChanged is the targeted
        // signal for album-filtered media grids (consumed in a later PR).
        self.emit(AlbumEvent::AlbumUpdated(album_id.clone()));
        self.emit(AlbumEvent::AlbumMediaChanged(album_id.clone()));
        Ok(())
    }

    pub async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        let mut tx = self.db.begin().await?;
        self.repo
            .remove_media_in_tx(&mut tx, album_id, media_ids)
            .await?;
        self.recorder
            .record(
                &mut tx,
                &Mutation::AlbumMediaRemoved {
                    album_id: album_id.clone(),
                    media_ids: media_ids.to_vec(),
                },
            )
            .await?;
        tx.commit().await?;
        self.emit(AlbumEvent::AlbumUpdated(album_id.clone()));
        self.emit(AlbumEvent::AlbumMediaChanged(album_id.clone()));
        Ok(())
    }

    pub async fn albums_containing_media(
        &self,
        media_ids: &[MediaId],
    ) -> Result<HashMap<AlbumId, usize>, LibraryError> {
        self.repo.containing_media(media_ids).await
    }

    pub async fn album_cover_media_ids(
        &self,
        album_id: &AlbumId,
        limit: u32,
    ) -> Result<Vec<MediaId>, LibraryError> {
        self.repo.cover_media_ids(album_id, limit).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::db::test_helpers::open_test_db;
    use crate::sync::outbox::NoOpRecorder;
    use tempfile::tempdir;

    pub(super) async fn make_service() -> (tempfile::TempDir, AlbumService) {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let recorder: Arc<dyn MutationRecorder> = Arc::new(NoOpRecorder);
        (dir, AlbumService::new(db, recorder))
    }

    // ── Transactional outbox (#655) ─────────────────────────────────

    use crate::library::media::repository::MediaRepository;
    use crate::library::recorder::tests::{outbox_count, FailingRecorder};

    async fn drain(rx: &mut mpsc::UnboundedReceiver<AlbumEvent>) -> Vec<AlbumEvent> {
        let mut events = Vec::new();
        while let Ok(Some(e)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            events.push(e);
        }
        events
    }

    #[tokio::test]
    async fn recorder_failure_rolls_back_album_writes() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let media_id = MediaId::new("m1".to_string());
        MediaRepository::new(db.clone())
            .insert(&crate::library::db::test_helpers::test_record(
                media_id.clone(),
            ))
            .await
            .unwrap();
        // An existing album, created through a working service.
        let ok = AlbumService::new(db.clone(), Arc::new(NoOpRecorder));
        let album = ok.create_album("Before").await.unwrap();

        let svc = AlbumService::new(db.clone(), Arc::new(FailingRecorder::always(db.clone())));
        let mut rx = svc.subscribe();

        assert!(svc.create_album("Ghost").await.is_err());
        assert!(svc.rename_album(&album, "After").await.is_err());
        assert!(svc
            .add_to_album(&album, std::slice::from_ref(&media_id))
            .await
            .is_err());
        assert!(svc.delete_album(&album).await.is_err());

        let albums = svc.list_albums().await.unwrap();
        assert_eq!(albums.len(), 1, "create must roll back, delete too");
        assert_eq!(albums[0].name, "Before", "rename must roll back");
        assert_eq!(albums[0].media_count, 0, "add-media must roll back");
        assert_eq!(outbox_count(&db).await, 0);
        assert!(drain(&mut rx).await.is_empty(), "no event for a rollback");
    }

    #[tokio::test]
    async fn recorder_failure_rolls_back_remove_from_album() {
        let dir = tempdir().unwrap();
        let db = open_test_db(dir.path()).await;
        let media_id = MediaId::new("m1".to_string());
        MediaRepository::new(db.clone())
            .insert(&crate::library::db::test_helpers::test_record(
                media_id.clone(),
            ))
            .await
            .unwrap();
        let ok = AlbumService::new(db.clone(), Arc::new(NoOpRecorder));
        let album = ok.create_album("Keep").await.unwrap();
        ok.add_to_album(&album, std::slice::from_ref(&media_id))
            .await
            .unwrap();

        let svc = AlbumService::new(db.clone(), Arc::new(FailingRecorder::always(db.clone())));
        assert!(svc
            .remove_from_album(&album, std::slice::from_ref(&media_id))
            .await
            .is_err());

        let got = svc.get_album(&album).await.unwrap().unwrap();
        assert_eq!(got.media_count, 1);
        assert_eq!(outbox_count(&db).await, 0);
    }
}

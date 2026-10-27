// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

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
    repo: AlbumRepository,
    recorder: Arc<dyn MutationRecorder>,
    events: EventEmitter<AlbumEvent>,
}

impl AlbumService {
    pub fn new(db: Database, recorder: Arc<dyn MutationRecorder>) -> Self {
        Self {
            repo: AlbumRepository::new(db),
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
        let id = self.repo.create(name).await?;
        if let Err(e) = self
            .recorder
            .record(&Mutation::AlbumCreated {
                id: id.clone(),
                name: name.to_string(),
            })
            .await
        {
            warn!(error = %e, "failed to record AlbumCreated mutation");
        }
        Ok(id)
    }

    pub async fn set_pinned(&self, id: &AlbumId, pinned: bool) -> Result<(), LibraryError> {
        self.repo.set_pinned(id, pinned).await
    }

    pub async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        self.repo.rename(id, name).await?;
        if let Err(e) = self
            .recorder
            .record(&Mutation::AlbumRenamed {
                id: id.clone(),
                name: name.to_string(),
            })
            .await
        {
            warn!(error = %e, "failed to record AlbumRenamed mutation");
        }
        Ok(())
    }

    pub async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError> {
        let external_id = self.repo.external_id(id).await.unwrap_or(None);
        self.repo.delete(id).await?;
        self.emit(AlbumEvent::AlbumRemoved(id.clone()));
        if let Err(e) = self
            .recorder
            .record(&Mutation::AlbumDeleted {
                id: id.clone(),
                external_id,
            })
            .await
        {
            warn!(error = %e, "failed to record AlbumDeleted mutation");
        }
        Ok(())
    }

    pub async fn add_to_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.repo.add_media(album_id, media_ids).await?;
        // Emit both for now: AlbumUpdated preserves the existing
        // AlbumClientV2 refresh path; AlbumMediaChanged is the targeted
        // signal for album-filtered media grids (consumed in a later PR).
        self.emit(AlbumEvent::AlbumUpdated(album_id.clone()));
        self.emit(AlbumEvent::AlbumMediaChanged(album_id.clone()));
        if let Err(e) = self
            .recorder
            .record(&Mutation::AlbumMediaAdded {
                album_id: album_id.clone(),
                media_ids: media_ids.to_vec(),
            })
            .await
        {
            warn!(error = %e, "failed to record AlbumMediaAdded mutation");
        }
        Ok(())
    }

    pub async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.repo.remove_media(album_id, media_ids).await?;
        self.emit(AlbumEvent::AlbumUpdated(album_id.clone()));
        self.emit(AlbumEvent::AlbumMediaChanged(album_id.clone()));
        if let Err(e) = self
            .recorder
            .record(&Mutation::AlbumMediaRemoved {
                album_id: album_id.clone(),
                media_ids: media_ids.to_vec(),
            })
            .await
        {
            warn!(error = %e, "failed to record AlbumMediaRemoved mutation");
        }
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
}

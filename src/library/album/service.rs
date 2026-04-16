use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

use super::event::AlbumEvent;
use super::model::{Album, AlbumId};
use super::repository::AlbumRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::{MediaCursor, MediaId, MediaItem};
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;

/// Album management service.
///
/// Holds an `mpsc::Sender<AlbumEvent>` to notify the client layer of
/// state changes. Call `subscribe()` once to obtain the receiver.
#[derive(Clone)]
pub struct AlbumService {
    repo: AlbumRepository,
    recorder: Arc<dyn MutationRecorder>,
    events_tx: mpsc::UnboundedSender<AlbumEvent>,
    /// Held so `subscribe()` can hand it out exactly once.
    events_rx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<AlbumEvent>>>>,
}

impl AlbumService {
    pub fn new(db: Database, recorder: Arc<dyn MutationRecorder>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            repo: AlbumRepository::new(db),
            recorder,
            events_tx: tx,
            events_rx: Arc::new(tokio::sync::Mutex::new(Some(rx))),
        }
    }

    /// Take the event receiver. Can only be called once — panics on
    /// subsequent calls.
    pub async fn subscribe(&self) -> mpsc::UnboundedReceiver<AlbumEvent> {
        self.events_rx
            .lock()
            .await
            .take()
            .expect("AlbumService::subscribe() called more than once")
    }

    /// Send an event, ignoring errors (no subscriber yet, or dropped).
    fn emit(&self, event: AlbumEvent) {
        let _ = self.events_tx.send(event);
    }

    // ── Sync upsert (pull from server, no outbox recording) ────────

    /// Insert or replace an album from the sync stream.
    pub async fn upsert_album(
        &self,
        id: &str,
        name: &str,
        created_at: i64,
        updated_at: i64,
        external_id: Option<&str>,
    ) -> Result<(), LibraryError> {
        let existed = self.repo.get_by_raw_id(id).await?.is_some();
        self.repo
            .upsert(id, name, created_at, updated_at, external_id)
            .await?;
        let album_id = AlbumId::from_raw(id.to_string());
        if existed {
            self.emit(AlbumEvent::AlbumUpdated(album_id));
        } else {
            self.emit(AlbumEvent::AlbumAdded(album_id));
        }
        Ok(())
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
        self.emit(AlbumEvent::AlbumUpdated(album_id.clone()));
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

    pub async fn list_album_media(
        &self,
        album_id: &AlbumId,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        self.repo.list_media(album_id, cursor, limit).await
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

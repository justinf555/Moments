use std::collections::HashMap;
use std::sync::Arc;

use tracing::warn;

use super::model::{Album, AlbumId};
use super::repository::AlbumRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::{MediaCursor, MediaId, MediaItem};
use crate::library::mutation::Mutation;
use crate::library::recorder::MutationRecorder;

/// Album management service.
#[derive(Clone)]
pub struct AlbumService {
    repo: AlbumRepository,
    recorder: Arc<dyn MutationRecorder>,
}

impl AlbumService {
    pub fn new(db: Database, recorder: Arc<dyn MutationRecorder>) -> Self {
        Self {
            repo: AlbumRepository::new(db),
            recorder,
        }
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
        self.repo
            .upsert(id, name, created_at, updated_at, external_id)
            .await
    }

    // ── Query methods ───────────────────────────────────────────────

    pub async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        self.repo.list().await
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

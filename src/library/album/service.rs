use std::collections::HashMap;
use std::sync::Arc;

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
    pub(crate) repo: AlbumRepository,
    recorder: Arc<dyn MutationRecorder>,
}

impl AlbumService {
    pub fn new(db: Database, recorder: Arc<dyn MutationRecorder>) -> Self {
        Self {
            repo: AlbumRepository::new(db),
            recorder,
        }
    }

    pub async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        self.repo.list().await
    }

    pub async fn create_album(&self, name: &str) -> Result<AlbumId, LibraryError> {
        let id = self.repo.create(name).await?;
        self.recorder
            .record(&Mutation::AlbumCreated {
                id: id.clone(),
                name: name.to_string(),
            })
            .await?;
        Ok(id)
    }

    pub async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        self.repo.rename(id, name).await?;
        self.recorder
            .record(&Mutation::AlbumRenamed {
                id: id.clone(),
                name: name.to_string(),
            })
            .await
    }

    pub async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError> {
        self.repo.delete(id).await?;
        self.recorder
            .record(&Mutation::AlbumDeleted { id: id.clone() })
            .await
    }

    pub async fn add_to_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.repo.add_media(album_id, media_ids).await?;
        self.recorder
            .record(&Mutation::AlbumMediaAdded {
                album_id: album_id.clone(),
                media_ids: media_ids.to_vec(),
            })
            .await
    }

    pub async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.repo.remove_media(album_id, media_ids).await?;
        self.recorder
            .record(&Mutation::AlbumMediaRemoved {
                album_id: album_id.clone(),
                media_ids: media_ids.to_vec(),
            })
            .await
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

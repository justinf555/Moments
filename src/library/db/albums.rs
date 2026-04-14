use std::collections::HashMap;

use crate::library::album::repository::AlbumRepository;
use crate::library::album::{Album, AlbumId, LibraryAlbums};
use crate::library::error::LibraryError;
use crate::library::media::{MediaCursor, MediaId, MediaItem};

use super::Database;

/// Delegate `LibraryAlbums` on `Database` to `AlbumRepository`.
///
/// This impl exists so that code which holds a `Database` (e.g. sync
/// tests, sync manager) can still call album methods directly. All SQL
/// lives in `AlbumRepository` — this is a thin forwarding layer.
#[async_trait::async_trait]
impl LibraryAlbums for Database {
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        AlbumRepository::new(self.clone()).list().await
    }

    async fn create_album(&self, name: &str) -> Result<AlbumId, LibraryError> {
        AlbumRepository::new(self.clone()).create(name).await
    }

    async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        AlbumRepository::new(self.clone()).rename(id, name).await
    }

    async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError> {
        AlbumRepository::new(self.clone()).delete(id).await
    }

    async fn add_to_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        AlbumRepository::new(self.clone())
            .add_media(album_id, media_ids)
            .await
    }

    async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        AlbumRepository::new(self.clone())
            .remove_media(album_id, media_ids)
            .await
    }

    async fn list_album_media(
        &self,
        album_id: &AlbumId,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        AlbumRepository::new(self.clone())
            .list_media(album_id, cursor, limit)
            .await
    }

    async fn albums_containing_media(
        &self,
        media_ids: &[MediaId],
    ) -> Result<HashMap<AlbumId, usize>, LibraryError> {
        AlbumRepository::new(self.clone())
            .containing_media(media_ids)
            .await
    }

    async fn album_cover_media_ids(
        &self,
        album_id: &AlbumId,
        limit: u32,
    ) -> Result<Vec<MediaId>, LibraryError> {
        AlbumRepository::new(self.clone())
            .cover_media_ids(album_id, limit)
            .await
    }
}

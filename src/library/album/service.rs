use std::collections::HashMap;

use async_trait::async_trait;

use super::model::{Album, AlbumId};
use super::repository::AlbumRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::{MediaCursor, MediaId, MediaItem};

/// Feature trait for album management.
///
/// The public contract consumed by the UI, sync extensions, and the
/// future Media Portal. Implementations must be `Send + Sync` so they
/// can be shared across the GTK and Tokio executors.
#[async_trait]
pub trait LibraryAlbums: Send + Sync {
    /// List all albums, ordered by most recently updated first.
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError>;

    /// Create a new album with the given name. Returns the new album's ID.
    async fn create_album(&self, name: &str) -> Result<AlbumId, LibraryError>;

    /// Rename an existing album.
    async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError>;

    /// Delete an album and all its media associations.
    /// Does not delete the media items themselves.
    async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError>;

    /// Add media items to an album. Duplicates are silently ignored.
    async fn add_to_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError>;

    /// Remove media items from an album.
    async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError>;

    /// List media in an album with keyset pagination.
    /// Excludes trashed items. Sorted by capture date (newest first).
    async fn list_album_media(
        &self,
        album_id: &AlbumId,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError>;

    /// For each album containing at least one of `media_ids`, return the
    /// count of how many are present. Used by the album picker to show
    /// "Already added" badges.
    async fn albums_containing_media(
        &self,
        media_ids: &[MediaId],
    ) -> Result<HashMap<AlbumId, usize>, LibraryError>;

    /// Return up to `limit` most recent media IDs for an album's cover mosaic.
    async fn album_cover_media_ids(
        &self,
        album_id: &AlbumId,
        limit: u32,
    ) -> Result<Vec<MediaId>, LibraryError>;
}

/// Local-first album service.
///
/// Implements [`LibraryAlbums`] by delegating to [`AlbumRepository`].
/// This is the canonical implementation — both the unified Library and
/// individual providers delegate to it.
#[derive(Clone)]
pub struct AlbumService {
    pub(crate) repo: AlbumRepository,
}

impl AlbumService {
    pub fn new(db: Database) -> Self {
        Self {
            repo: AlbumRepository::new(db),
        }
    }
}

#[async_trait]
impl LibraryAlbums for AlbumService {
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        self.repo.list().await
    }

    async fn create_album(&self, name: &str) -> Result<AlbumId, LibraryError> {
        self.repo.create(name).await
    }

    async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        self.repo.rename(id, name).await
    }

    async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError> {
        self.repo.delete(id).await
    }

    async fn add_to_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.repo.add_media(album_id, media_ids).await
    }

    async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.repo.remove_media(album_id, media_ids).await
    }

    async fn list_album_media(
        &self,
        album_id: &AlbumId,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        self.repo.list_media(album_id, cursor, limit).await
    }

    async fn albums_containing_media(
        &self,
        media_ids: &[MediaId],
    ) -> Result<HashMap<AlbumId, usize>, LibraryError> {
        self.repo.containing_media(media_ids).await
    }

    async fn album_cover_media_ids(
        &self,
        album_id: &AlbumId,
        limit: u32,
    ) -> Result<Vec<MediaId>, LibraryError> {
        self.repo.cover_media_ids(album_id, limit).await
    }
}

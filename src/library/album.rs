use std::collections::HashMap;

use async_trait::async_trait;

use super::error::LibraryError;
use super::media::{MediaCursor, MediaId, MediaItem};

/// Unique identifier for an album (UUID v4).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AlbumId(String);

impl AlbumId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_raw(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for AlbumId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Summary of an album, including aggregate counts.
#[derive(Debug, Clone)]
pub struct Album {
    pub id: AlbumId,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Number of (non-trashed) media items in this album.
    pub media_count: u32,
    /// Most recently added media item — used as the album cover thumbnail.
    pub cover_media_id: Option<MediaId>,
}

/// Feature trait for album management.
///
/// Implemented by every backend that supports albums. `Database` implements
/// the SQL logic; `LocalLibrary` delegates to its `Database`.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_id_new_generates_uuid() {
        let id = AlbumId::new();
        assert_eq!(id.as_str().len(), 36); // UUID v4 format: 8-4-4-4-12
        assert!(id.as_str().contains('-'));
    }

    #[test]
    fn album_id_display() {
        let id = AlbumId::from_raw("test-id".to_string());
        assert_eq!(format!("{id}"), "test-id");
    }

    #[test]
    fn album_ids_are_unique() {
        let id1 = AlbumId::new();
        let id2 = AlbumId::new();
        assert_ne!(id1, id2);
    }
}

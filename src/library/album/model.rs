// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use crate::library::media::MediaId;

/// Unique identifier for an album (UUID v4).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AlbumId(String);

impl AlbumId {
    /// Generate a new random UUID v4 identity (hyphenated, unlike `MediaId`;
    /// existing album rows use this format).
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Wrap an existing ID string (from database or sync stream).
    pub fn new(id: String) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
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
    /// Whether this album is pinned to the sidebar.
    pub is_pinned: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_id_new_generates_uuid() {
        let id = AlbumId::generate();
        assert_eq!(id.as_str().len(), 36); // UUID v4 format: 8-4-4-4-12
        assert!(id.as_str().contains('-'));
    }

    #[test]
    fn album_id_display() {
        let id = AlbumId::new("test-id".to_string());
        assert_eq!(format!("{id}"), "test-id");
    }

    #[test]
    fn album_ids_are_unique() {
        let id1 = AlbumId::generate();
        let id2 = AlbumId::generate();
        assert_ne!(id1, id2);
    }
}

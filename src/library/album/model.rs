use super::super::media::MediaId;

/// Unique identifier for an album (UUID v4).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AlbumId(String);

#[allow(clippy::new_without_default)]
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
    /// Whether this album is pinned to the sidebar.
    pub is_pinned: bool,
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

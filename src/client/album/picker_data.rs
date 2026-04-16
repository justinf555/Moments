//! Data types for the album picker dialog.

use crate::library::album::AlbumId;

/// View-model for a single album row in the picker.
#[derive(Debug)]
pub struct AlbumEntry {
    /// Album identifier.
    pub id: AlbumId,
    /// Album display name.
    pub name: String,
    /// Number of (non-trashed) media items in this album.
    pub media_count: u32,
    /// Pre-decoded thumbnail pixels (RGBA, width, height).
    pub thumbnail_rgba: Option<(Vec<u8>, u32, u32)>,
    /// How many of the selected media items are already in this album.
    pub already_added_count: usize,
}

//! Data types for the album picker dialog.
//!
//! These are plain structs computed by the caller before presenting the
//! dialog. The dialog never imports `Library` or performs async queries.

use crate::library::album::AlbumId;

/// All data needed to populate the album picker dialog.
#[derive(Debug)]
pub struct AlbumPickerData {
    /// Albums to display, pre-sorted by `updated_at` DESC.
    pub albums: Vec<AlbumEntry>,
    /// Media IDs being added (the current selection).
    pub media_ids: Vec<crate::library::media::MediaId>,
}

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
    /// Decoded on the Tokio thread to avoid blocking the GTK thread.
    pub thumbnail_rgba: Option<(Vec<u8>, u32, u32)>,
    /// How many of the selected media items are already in this album.
    /// `0` = none added, `N` = N of the selection are already present.
    pub already_added_count: usize,
}

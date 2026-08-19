// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

/// Opaque identity for every media asset in the library.
///
/// A UUID v4 stored as a 32-char lowercase hex string (no dashes).
/// Generated via [`MediaId::generate`] for local imports; loaded from
/// the database via [`MediaId::new`] for existing records and sync.
///
/// Content-based deduplication uses the separate `content_hash` field
/// on [`MediaRecord`], not the ID itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaId(String);

impl MediaId {
    /// Generate a new random UUID v4 identity (32-char hex, no dashes).
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    /// Wrap an existing ID string (from database or sync stream).
    pub fn new(id: String) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MediaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Media type stored in the `media.media_type` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum MediaType {
    Image = 0,
    Video = 1,
}

/// The read model returned by [`super::service::MediaService::list_media`].
///
/// Contains everything the photo grid needs to display a cell — id for
/// thumbnail lookup, dimensions for aspect-ratio placeholder, and capture
/// time for grouping. Full EXIF detail is in `media_metadata` and is
/// fetched separately when the detail view needs it.
#[derive(Debug, Clone)]
pub struct MediaItem {
    pub id: MediaId,
    /// Capture time as the capture-local wall clock, encoded as seconds
    /// since the epoch. See [`MediaRecord::taken_at`]. `None` if unavailable.
    pub taken_at: Option<i64>,
    pub imported_at: i64,
    pub original_filename: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// EXIF orientation tag (1–8).
    pub orientation: u8,
    pub media_type: MediaType,
    pub is_favorite: bool,
    pub is_trashed: bool,
    /// Unix timestamp when the item was trashed. `None` if not trashed.
    pub trashed_at: Option<i64>,
    /// Video duration in milliseconds. `None` for images.
    pub duration_ms: Option<u64>,
    /// Identifier of the stack this asset belongs to, if any.
    /// `None` for un-stacked items. Set on every member of a stack
    /// (including the primary). The grid filters non-primary members
    /// out via a `LEFT JOIN stacks` clause; clients can use this to
    /// surface a "stacked" badge on the primary.
    pub stack_id: Option<String>,
    /// True if this asset is a Moments-produced render (the rendered
    /// JPEG sibling of an edited original). Phase C (#224): the grid
    /// filter swaps these out for the original sibling so the user
    /// sees the editable artifact rather than the flat render.
    pub is_moments_render: bool,
}

/// A stack of related assets on the Immich server (e.g. a panorama
/// burst, or a Moments-rendered edit alongside its original).
///
/// Issue #224: Immich exposes stacks via the `StacksV1` sync stream.
/// Locally we cache them as a peer table so the timeline can collapse
/// stacked siblings to the primary, and so push-side stack mutations
/// have a stable identifier to mutate.
///
/// Note: this struct intentionally does NOT carry `last_seen_at`.
/// Heartbeat (#628) is a local concern managed via
/// [`super::repository::MediaRepository::bump_stack_last_seen_at`] /
/// [`super::repository::MediaRepository::ensure_stack_stub`] — the
/// server's `SyncStackV1` payload doesn't include it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    /// Server-assigned UUID. Stable across pulls.
    pub id: String,
    /// The asset that surfaces in the timeline as the visible
    /// representative of this stack.
    pub primary_asset_id: MediaId,
}

/// Filter for [`super::service::MediaService::list_media`] queries.
///
/// Not `Copy` because `Album` holds an `AlbumId` (heap-allocated String).
/// Use `RefCell<MediaFilter>` instead of `Cell<MediaFilter>` in UI models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaFilter {
    /// All media items (excludes trashed).
    All,
    /// Only items marked as favourite (excludes trashed).
    Favorites,
    /// Only trashed items.
    Trashed,
    /// Items imported after `since` (Unix timestamp). Excludes trashed.
    /// Sorted by `imported_at DESC` instead of `taken_at`.
    RecentImports { since: i64 },
    /// Items belonging to a specific album. Excludes trashed.
    Album {
        album_id: crate::library::album::AlbumId,
    },
    /// Items containing a specific person's face. Excludes trashed.
    Person {
        person_id: crate::library::faces::PersonId,
    },
}

impl MediaFilter {
    /// Check whether a [`MediaItem`] belongs in this filtered view.
    ///
    /// Returns `false` for `Album` — album membership requires a DB query
    /// and cannot be determined from the item alone.
    pub fn matches(&self, item: &MediaItem) -> bool {
        match self {
            MediaFilter::All => !item.is_trashed,
            MediaFilter::Favorites => item.is_favorite && !item.is_trashed,
            MediaFilter::Trashed => item.is_trashed,
            MediaFilter::RecentImports { since } => !item.is_trashed && item.imported_at > *since,
            MediaFilter::Album { .. } => false,
            MediaFilter::Person { .. } => false,
        }
    }

    /// Returns `true` if [`matches`] can authoritatively decide membership
    /// from the item data alone, without a DB query.
    ///
    /// Returns `false` for `Album` and `Person` — membership for those views
    /// requires a join and is never knowable from the [`MediaItem`] fields.
    pub fn supports_inline_match(&self) -> bool {
        !matches!(self, MediaFilter::Album { .. } | MediaFilter::Person { .. })
    }
}

/// Opaque cursor for keyset pagination in [`super::service::MediaService::list_media`].
///
/// Encodes the position of the last item seen so the next page continues
/// exactly where the previous one left off — no `OFFSET` scans.
///
/// `sort_key` is `COALESCE(taken_at, 0)` so items without EXIF dates
/// sort to the end of the timeline.
#[derive(Debug, Clone)]
pub struct MediaCursor {
    /// `COALESCE(taken_at, 0)` of the last seen item.
    pub sort_key: i64,
    /// `id` of the last seen item — tiebreaker within the same timestamp.
    pub id: MediaId,
}

/// A row in the `media` table.
#[derive(Debug, Clone)]
pub struct MediaRecord {
    pub id: MediaId,
    /// SHA-1 content hash, base64-encoded — matches Immich's wire format.
    /// Used for dedup, not identity. Local imports compute this from the
    /// file bytes; sync-origin rows take it from the Immich `AssetV1`
    /// stream's `checksum` field. Symmetric format means a file pulled
    /// from Immich and then re-imported locally is rejected as a dup
    /// without needing to download the original.
    pub content_hash: Option<String>,
    /// Immich server UUID. Set when synced with an Immich server.
    pub external_id: Option<String>,
    /// Path relative to the bundle's `originals/` directory.
    pub relative_path: String,
    pub original_filename: String,
    pub file_size: i64,
    /// Unix timestamp (seconds since epoch).
    pub imported_at: i64,
    pub media_type: MediaType,
    /// Capture time as the *capture-local wall clock* — the reading on the
    /// camera's own clock — encoded as seconds since the epoch.
    ///
    /// Deliberately not an instant in UTC: photo libraries show the time the
    /// shutter fired, not that time re-expressed wherever the viewer happens
    /// to be. The EXIF extractor keeps `DateTimeOriginal` verbatim and the
    /// Immich sync handler prefers the server's `localDateTime`, so both
    /// import paths land on the same convention. Issue #549.
    ///
    /// `None` if unavailable.
    pub taken_at: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// EXIF orientation tag (1–8). Defaults to 1 (normal).
    pub orientation: u8,
    /// Video duration in milliseconds. `None` for images.
    pub duration_ms: Option<u64>,
    /// Whether the asset is marked as favourite.
    pub is_favorite: bool,
    /// Whether the asset is trashed.
    pub is_trashed: bool,
    /// Unix timestamp when the item was trashed. `None` if not trashed.
    pub trashed_at: Option<i64>,
    /// True if this row was inserted by Phase C save flow as the
    /// rendered sibling of an edited asset. Toggled on by the editor
    /// at save time and on the pull side when an asset arrives with
    /// the `moments-edit` tag.
    pub is_moments_render: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_32_char_hex() {
        let id = MediaId::generate();
        assert_eq!(id.as_str().len(), 32);
        assert!(id.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generated_ids_are_unique() {
        let id1 = MediaId::generate();
        let id2 = MediaId::generate();
        assert_ne!(id1, id2);
    }

    #[test]
    fn new_wraps_existing_string() {
        let id = MediaId::new("abc123".to_string());
        assert_eq!(id.as_str(), "abc123");
    }
}

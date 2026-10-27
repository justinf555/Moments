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

/// Where an asset is allowed to appear, stored in `media.visibility`.
///
/// Mirrors Immich's `AssetVisibility` (issue #695). Locally imported
/// media is always [`MediaVisibility::Timeline`] (the column default),
/// so only the Immich pull path ever writes anything else.
///
/// | Value      | All, Favourites, Recent Imports | Albums, People, Trash |
/// |------------|---------------------------------|-----------------------|
/// | `Timeline` | shown                           | shown                 |
/// | `Archive`  | hidden                          | shown                 |
/// | `Hidden`   | hidden                          | hidden                |
/// | `Locked`   | hidden                          | hidden                |
///
/// `Locked` stays hidden everywhere until a locked-folder view (#464)
/// exists. `Hidden` is Immich-internal (e.g. the motion half of a Live
/// Photo, #45) and is never meant to be browsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaVisibility {
    /// Normal asset, shown in the timeline.
    #[default]
    Timeline,
    /// Archived: out of the timeline, still in albums and people.
    Archive,
    /// Immich-internal asset, never listed.
    Hidden,
    /// Locked-folder asset (PIN-protected on the server), never listed.
    Locked,
}

impl MediaVisibility {
    /// Visibilities that may appear in *some* listing (albums, people,
    /// trash). Anything outside this set is excluded from every query
    /// that lists media.
    pub const LISTABLE: &'static [MediaVisibility] =
        &[MediaVisibility::Timeline, MediaVisibility::Archive];

    /// Visibilities shown in the main timeline views.
    pub const TIMELINE: &'static [MediaVisibility] = &[MediaVisibility::Timeline];

    const ALL: [MediaVisibility; 4] = [
        MediaVisibility::Timeline,
        MediaVisibility::Archive,
        MediaVisibility::Hidden,
        MediaVisibility::Locked,
    ];

    /// The value stored in `media.visibility`. Identical to Immich's
    /// wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            MediaVisibility::Timeline => "timeline",
            MediaVisibility::Archive => "archive",
            MediaVisibility::Hidden => "hidden",
            MediaVisibility::Locked => "locked",
        }
    }

    /// Parse a stored or wire value, ignoring ASCII case. `None` for
    /// anything unrecognised.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|v| v.as_str().eq_ignore_ascii_case(s))
    }

    /// Parse, mapping unknown values to [`MediaVisibility::Hidden`].
    ///
    /// A value we don't understand may be a new privacy tier, so it is
    /// treated as "never list" rather than "show in the timeline".
    pub fn parse_or_hidden(s: &str) -> Self {
        Self::parse(s).unwrap_or(MediaVisibility::Hidden)
    }

    /// `true` if this asset may appear in at least one listing.
    pub fn is_listable(self) -> bool {
        Self::LISTABLE.contains(&self)
    }
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
    /// Where the asset may appear. Issue #695.
    pub visibility: MediaVisibility,
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
        if !self.admitted_visibilities().contains(&item.visibility) {
            return false;
        }
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

    /// The asset visibilities this view admits. Issue #695.
    ///
    /// The single source of truth for visibility filtering: [`Self::matches`]
    /// checks it in memory and `MediaRepository` turns the same slice into
    /// its SQL predicate, so the two can't drift.
    ///
    /// The main views show only timeline assets. Albums, people and trash
    /// also show archived ones (Immich's semantics). Hidden and locked
    /// assets are in neither set.
    pub fn admitted_visibilities(&self) -> &'static [MediaVisibility] {
        match self {
            MediaFilter::All | MediaFilter::Favorites | MediaFilter::RecentImports { .. } => {
                MediaVisibility::TIMELINE
            }
            MediaFilter::Trashed | MediaFilter::Album { .. } | MediaFilter::Person { .. } => {
                MediaVisibility::LISTABLE
            }
        }
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

/// Aggregate library statistics for the preferences overview.
#[derive(Debug, Clone, Default)]
pub struct LibraryStats {
    pub photo_count: u64,
    pub video_count: u64,
    pub album_count: u64,
    pub total_file_size: u64,
    /// Number of items currently in the trash.
    pub trashed_count: u64,
    /// Disk usage of the originals cache (Immich only), in bytes.
    pub cache_used_bytes: u64,
    /// People count (named, non-hidden).
    pub people_count: u64,
    /// Server-side statistics (Immich only).
    pub server: Option<ServerStats>,
}

/// Statistics from the Immich server (populated via API calls).
#[derive(Debug, Clone)]
pub struct ServerStats {
    /// Server photo count for the authenticated user.
    pub server_photos: u64,
    /// Server video count for the authenticated user.
    pub server_videos: u64,
    /// Server total disk size in bytes.
    pub disk_size: u64,
    /// Server used disk space in bytes.
    pub disk_use: u64,
    /// Server disk usage percentage (0–100).
    pub disk_usage_percentage: f64,
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

    #[test]
    fn visibility_round_trips_through_str() {
        for v in MediaVisibility::ALL {
            assert_eq!(MediaVisibility::parse(v.as_str()), Some(v));
        }
        assert_eq!(
            MediaVisibility::parse("LOCKED"),
            Some(MediaVisibility::Locked)
        );
    }

    /// An unrecognised server value must never land in the timeline.
    #[test]
    fn unknown_visibility_maps_to_hidden() {
        assert_eq!(MediaVisibility::parse("vault"), None);
        assert_eq!(
            MediaVisibility::parse_or_hidden("vault"),
            MediaVisibility::Hidden
        );
        assert!(!MediaVisibility::parse_or_hidden("").is_listable());
    }

    fn item(visibility: MediaVisibility, is_favorite: bool, is_trashed: bool) -> MediaItem {
        MediaItem {
            id: MediaId::new("x".to_string()),
            taken_at: None,
            imported_at: 100,
            original_filename: "x.jpg".to_string(),
            width: None,
            height: None,
            orientation: 1,
            media_type: MediaType::Image,
            is_favorite,
            is_trashed,
            trashed_at: None,
            duration_ms: None,
            stack_id: None,
            is_moments_render: false,
            visibility,
        }
    }

    /// Issue #695: the in-memory match (used by the client to patch open
    /// grids) agrees with the visibility table on `MediaVisibility`.
    #[test]
    fn filter_matches_respects_visibility() {
        use MediaVisibility::*;
        let recent = MediaFilter::RecentImports { since: 0 };
        for v in [Timeline, Archive, Hidden, Locked] {
            let timeline = v == Timeline;
            assert_eq!(MediaFilter::All.matches(&item(v, false, false)), timeline);
            assert_eq!(
                MediaFilter::Favorites.matches(&item(v, true, false)),
                timeline
            );
            assert_eq!(recent.matches(&item(v, false, false)), timeline);
            assert_eq!(
                MediaFilter::Trashed.matches(&item(v, false, true)),
                v.is_listable()
            );
        }
    }
}

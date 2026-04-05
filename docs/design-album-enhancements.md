# Design: Album Enhancements — Pinning, Covers, Metadata

**Status:** Draft
**Issue:** TBD

---

## Problem

The `Album` model is missing several fields that Immich provides and that users
expect from a photo management app. A full audit of the Immich
`AlbumResponseDto` (via OpenAPI spec) reveals the following gaps:

### Immich AlbumResponseDto field inventory

| Field | Type | In Moments? | Action |
|-------|------|-------------|--------|
| `id` | string | Yes | — |
| `albumName` | string | Yes | — |
| `createdAt` | datetime | Yes | — |
| `updatedAt` | datetime | Yes | — |
| `assetCount` | integer | Yes (computed locally) | — |
| `albumThumbnailAssetId` | string, nullable | **No** | Add — explicit cover |
| `description` | string | **No** | Add — display + edit |
| `order` | `asc` / `desc` | **No** | Add — per-album sort |
| `isActivityEnabled` | boolean | No | Skip — server-only feature |
| `hasSharedLink` | boolean | No | Skip — no shared link UI |
| `shared` | boolean | No | Skip — no multi-user sharing |
| `ownerId` | string | No | Skip — single-user client |
| `owner` | UserResponseDto | No | Skip — single-user client |
| `albumUsers` | array | No | Skip — no multi-user sharing |
| `startDate` | datetime | No | Skip — derivable from assets |
| `endDate` | datetime | No | Skip — derivable from assets |
| `lastModifiedAssetTimestamp` | datetime | No | Skip — derivable |
| `assets` | array | No | Skip — synced via AlbumToAsset |
| `contributorCounts` | array | No | Skip — no multi-user sharing |

Beyond the sync gaps, pinning is also misplaced:

1. **Pinned albums are stored in GSettings** as an array of strings with an
   arbitrary max-5 limit. This is a UI preference mechanism being used for what
   is really album metadata. Users who want to pin 10+ albums are blocked by a
   hard cap that has no technical justification.

2. **Album covers are always auto-selected** as the most recently added media
   item. Immich supports an explicit `albumThumbnailAssetId` field on albums,
   but Moments ignores it during sync. Users cannot choose which photo
   represents an album.

3. **Album descriptions and sort order are not synced.** Immich albums have
   `description` and `order` fields that Moments does not store or display.

### Immich UpdateAlbumDto (writable fields via PATCH /albums/{id})

| Field | Type | Notes |
|-------|------|-------|
| `albumName` | string | Already supported |
| `albumThumbnailAssetId` | uuid string | Set explicit cover |
| `description` | string | Set description |
| `isActivityEnabled` | boolean | Skip — server-only |
| `order` | `asc` / `desc` | Per-album asset sort order |

## Current state

### Pinning

- GSettings key `pinned-album-ids` (type `as`, max 5)
- Sidebar reads on startup, writes on pin/unpin
- `Album` struct and DB schema have no pinned field
- Pin/unpin actions in album grid context menu and sidebar context menu

### Covers

- `cover_media_id` is computed at query time via a subquery:
  `SELECT media_id FROM album_media WHERE album_id = ? ORDER BY added_at DESC LIMIT 1`
- `album_cover_media_ids()` returns up to N recent media for the mosaic display
- `SyncAlbumV1` has no cover field — the Immich `albumThumbnailAssetId` is
  not synced
- No UI to set a custom cover

### Descriptions and sort order

- `SyncAlbumV1` has no `description` or `order` field
- `Album` struct has neither field
- Immich albums have `description` (string) and `order` (`asc` / `desc`)
- All album views currently sort by `added_at DESC` — no per-album override

## Architecture

### Database changes

One migration adds five columns to the `albums` table:

```sql
ALTER TABLE albums ADD COLUMN is_pinned       INTEGER NOT NULL DEFAULT 0;
ALTER TABLE albums ADD COLUMN pinned_position  INTEGER;
ALTER TABLE albums ADD COLUMN cover_media_id   TEXT    REFERENCES media(id)
                                                      ON DELETE SET NULL;
ALTER TABLE albums ADD COLUMN description     TEXT    NOT NULL DEFAULT '';
ALTER TABLE albums ADD COLUMN sort_order      TEXT    NOT NULL DEFAULT 'desc';

CREATE INDEX idx_albums_pinned ON albums(is_pinned, pinned_position);
```

- `is_pinned` + `pinned_position` — replaces GSettings. Position is a sparse
  integer for ordering (10, 20, 30...) so inserts don't require renumbering.
  `NULL` when not pinned.
- `cover_media_id` — explicit cover choice. `NULL` means "auto-select" (current
  behaviour). `ON DELETE SET NULL` falls back to auto when the cover photo is
  deleted.
- `description` — free-text, empty by default.
- `sort_order` — `"asc"` or `"desc"`. Default `"desc"` matches current
  behaviour (newest first). Maps to Immich's `order` field.

### Album struct changes

```rust
pub struct Album {
    pub id: AlbumId,
    pub name: String,
    pub description: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub media_count: u32,
    /// Explicit cover, or auto-selected most-recent if None.
    pub cover_media_id: Option<MediaId>,
    /// Whether the album is pinned to the sidebar.
    pub is_pinned: bool,
    /// Display order among pinned albums (lower = higher).
    pub pinned_position: Option<i32>,
    /// Per-album asset sort order ("asc" = oldest first, "desc" = newest first).
    pub sort_order: AlbumSortOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlbumSortOrder {
    Asc,
    #[default]
    Desc,
}
```

### LibraryAlbums trait additions

```rust
/// Set or clear the explicit cover photo for an album.
async fn set_album_cover(
    &self,
    album_id: &AlbumId,
    media_id: Option<&MediaId>,
) -> Result<(), LibraryError>;

/// Pin or unpin an album from the sidebar.
async fn set_album_pinned(
    &self,
    album_id: &AlbumId,
    pinned: bool,
) -> Result<(), LibraryError>;

/// Reorder a pinned album to a new position.
async fn reorder_pinned_album(
    &self,
    album_id: &AlbumId,
    new_position: i32,
) -> Result<(), LibraryError>;
```

### Cover resolution

The `list_albums` query changes to prefer the explicit cover, falling back to
auto-selection:

```sql
SELECT a.id, a.name, a.description, a.created_at, a.updated_at,
       a.is_pinned, a.pinned_position, a.sort_order,
       COUNT(am.media_id) as media_count,
       COALESCE(
           a.cover_media_id,
           (SELECT am2.media_id FROM album_media am2
            JOIN media m ON m.id = am2.media_id AND m.is_trashed = 0
            WHERE am2.album_id = a.id
            ORDER BY am2.added_at DESC LIMIT 1)
       ) as cover_media_id
FROM albums a
LEFT JOIN album_media am ON am.album_id = a.id
  AND am.media_id IN (SELECT id FROM media WHERE is_trashed = 0)
GROUP BY a.id
ORDER BY a.updated_at DESC
```

### Immich sync changes

**`SyncAlbumV1`** — add optional fields:

```rust
#[derive(Debug, Deserialize)]
pub(crate) struct SyncAlbumV1 {
    pub id: String,
    pub name: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "albumThumbnailAssetId")]
    pub cover_asset_id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Per-album asset sort order: "asc" or "desc".
    #[serde(default)]
    pub order: Option<String>,
}
```

All new fields are `Option` with `serde(default)` so older Immich servers that
don't send them won't break deserialization.

**`handle_album()`** — pass cover and description through to the DB upsert.

**Immich write path** — `PATCH /albums/{id}` supports these writable fields:

```json
{
  "albumName": "string",
  "albumThumbnailAssetId": "uuid",
  "description": "string",
  "order": "asc | desc"
}
```

When a user changes the cover, description, or sort order on an Immich-backed
album, call `PATCH /albums/{id}` with the relevant field.

### Pinning migration from GSettings

On first launch after the migration:

1. Read `pinned-album-ids` from GSettings
2. For each ID that exists in the DB, set `is_pinned = 1` and assign
   sequential `pinned_position` values (10, 20, 30...)
3. Clear the GSettings key (or leave it — it becomes inert)

This runs once in `application.rs` after `library.open()`, guarded by a
`pinned-migrated` boolean GSettings key.

### Sidebar changes

The sidebar currently calls `settings.strv("pinned-album-ids")` at startup
and manages an in-memory `pinned_ids: Vec<String>`. After migration:

- `load_pinned_albums()` queries `list_albums()` and filters
  `is_pinned == true`, ordered by `pinned_position`
- `pin_album()` / `unpin_album()` call the library trait methods instead of
  writing to GSettings
- Remove the max-5 limit — the sidebar can show as many pinned albums as the
  user wants (scrollable if needed)
- `is_pinned()` checks the album data, not an in-memory vec

### Album card cover

The album grid card already supports three display modes:

- **Placeholder** — no photos (folder icon)
- **Single** — 1-3 photos (first cover fills the frame)
- **Mosaic** — 4+ photos (2x2 grid)

When an explicit cover is set, the card should always use **Single** mode with
that cover photo, regardless of album size. The mosaic remains the default for
albums without an explicit cover.

### UI for setting a cover

**Photo viewer overflow menu** — when viewing a photo that belongs to albums,
add "Set as Album Cover" item. If the photo belongs to multiple albums, show a
submenu listing album names.

**Album grid context menu** — no change needed. The cover is set from the
viewer, not the grid.

## Implementation plan

### Phase 1: Database + pinning migration

1. Add migration 015 with the new columns
2. Update `Album` struct, `list_albums` query, `upsert_album`
3. Add `set_album_pinned()`, `reorder_pinned_album()` to trait + DB impl
4. Add one-time GSettings migration in `application.rs`
5. Update sidebar to query DB instead of GSettings
6. Remove max-5 limit
7. Regenerate `.sqlx/` snapshot

### Phase 2: Covers

1. Add `set_album_cover()` to trait + DB impl
2. Update `list_albums` query to use `COALESCE` resolution
3. Update `SyncAlbumV1` with `cover_asset_id`
4. Update `handle_album()` sync handler
5. Add Immich write path (`PATCH /albums/{id}` with `albumThumbnailAssetId`)
6. Update album card to use single mode for explicit covers
7. Add "Set as Album Cover" to viewer overflow menu

### Phase 3: Description and sort order

1. Add `description` and `order` to `SyncAlbumV1`
2. Update `handle_album()` to persist both fields
3. Display description in album header (below album name)
4. Add description edit UI (inline rename already exists — extend to description)
5. Wire `sort_order` into `list_album_media()` query (`ORDER BY added_at`
   direction based on album's `sort_order`)
6. Add sort order toggle in album view header (same pattern as album grid sort)
7. Add Immich write path for description and order changes

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| GSettings migration fails mid-way | Migration is idempotent — re-running sets the same values |
| Immich server doesn't send cover/description fields | `Option` + `serde(default)` — graceful fallback |
| Explicit cover deleted from library | `ON DELETE SET NULL` falls back to auto-selection |
| Sparse position integers overflow with many reorders | Compact positions periodically (renumber 10, 20, 30...) |
| Old Moments version reads DB after migration | New columns have defaults — old code ignores them |

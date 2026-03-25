# Face Integration Design (Immich)

**Issue:** [#178](https://github.com/justinf555/Moments/issues/178)
**Status:** Proposed
**Date:** 2026-03-25

## Overview

Integrate Immich's face detection and people management into Moments. Immich performs face detection and recognition server-side using machine learning — Moments consumes this data via the sync stream and presents it in the UI.

This is an **Immich-only feature**. The local backend has no face detection capability and returns empty results for all `LibraryFaces` methods.

## Core Principle: Sync, Don't Compute

Immich handles all face detection, recognition, clustering, and thumbnail generation. Moments:

1. **Syncs** person and face data via the existing `POST /sync/stream` endpoint
2. **Caches** person records and face-to-asset mappings in the local SQLite database
3. **Downloads** person thumbnails (face crops) alongside asset thumbnails
4. **Displays** a "People" section in the sidebar and a per-person grid view

No ML models, no face detection, no image processing — just data sync and UI.

## Architecture

```
┌──────────────┐                              ┌──────────────────┐
│  GTK UI      │   PersonSynced event         │  SyncManager     │
│              │ ◄─────────────────────────── │                  │
│  Sidebar:    │                               │  Now syncs:      │
│  "People"    │   reads from local DB         │  - AssetsV1      │
│  section     │ ────────────────────►  ┌──── │  - AssetExifsV1  │
│              │                        │     │  - AlbumsV1      │
│  Person      │                        │     │  - AlbumToAssetsV1│
│  grid view   │                   Database   │  + PeopleV1       │
└──────────────┘                   (SQLite)   │  + AssetFacesV1   │
                                   ┌────┘     └────────┬──────────┘
                                   │                   │
                                   │  people           │  Face thumbnail
                                   │  asset_faces      │  downloader
                                   │  tables           │  (reuses existing
                                   │                   │   thumbnail worker)
                                   └──────┬────────────┘
                                          │
                                          ▼
                                   ┌──────────────┐
                                   │ Immich Server │
                                   │              │
                                   │ ML pipeline: │
                                   │ detect →     │
                                   │ recognise →  │
                                   │ cluster      │
                                   └──────────────┘
```

## Immich API

### Sync Stream Entity Types

Adding `"PeopleV1"` and `"AssetFacesV1"` to the sync stream request causes the server to include person and face records in the same NDJSON stream we already process.

**PersonV1** (upsert):
```json
{
  "id": "uuid",
  "name": "Alice",
  "birthDate": null,
  "isHidden": false,
  "isFavorite": false,
  "color": null,
  "faceAssetId": "uuid-of-representative-face",
  "createdAt": "2024-01-01T00:00:00Z",
  "updatedAt": "2024-06-15T12:00:00Z",
  "ownerId": "uuid"
}
```

**PersonDeleteV1** (delete):
```json
{ "personId": "uuid" }
```

**AssetFaceV1** (upsert):
```json
{
  "id": "uuid",
  "assetId": "uuid",
  "personId": "uuid-or-null",
  "imageWidth": 4032,
  "imageHeight": 3024,
  "boundingBoxX1": 1200,
  "boundingBoxY1": 800,
  "boundingBoxX2": 1600,
  "boundingBoxY2": 1300,
  "sourceType": "MachineLearning"
}
```

**AssetFaceDeleteV1** (delete):
```json
{ "assetFaceId": "uuid" }
```

### REST Endpoints (used for thumbnails and writes)

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/people/{id}/thumbnail` | Face crop thumbnail (250×250 JPEG) |
| `PUT` | `/people/{id}` | Rename person, set hidden/favorite |
| `POST` | `/people/{id}/merge` | Merge two people |
| `PUT` | `/faces/{id}` | Reassign a face to a different person |

Thumbnails are server-generated 250×250 JPEG crops. They are fetched via `GET /people/{id}/thumbnail` — same pattern as asset thumbnails but stored in a separate `people/` directory.

## Database Schema

### Migration 012: `create_people`

```sql
CREATE TABLE IF NOT EXISTS people (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL DEFAULT '',
    birth_date      TEXT,
    is_hidden       INTEGER NOT NULL DEFAULT 0,
    is_favorite     INTEGER NOT NULL DEFAULT 0,
    color           TEXT,
    face_asset_id   TEXT,
    face_count      INTEGER NOT NULL DEFAULT 0,
    synced_at       INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_people_name ON people(name);
CREATE INDEX idx_people_face_count ON people(face_count DESC);
```

### Migration 013: `create_asset_faces`

```sql
CREATE TABLE IF NOT EXISTS asset_faces (
    id              TEXT PRIMARY KEY,
    asset_id        TEXT NOT NULL,
    person_id       TEXT,
    image_width     INTEGER NOT NULL DEFAULT 0,
    image_height    INTEGER NOT NULL DEFAULT 0,
    bbox_x1         INTEGER NOT NULL DEFAULT 0,
    bbox_y1         INTEGER NOT NULL DEFAULT 0,
    bbox_x2         INTEGER NOT NULL DEFAULT 0,
    bbox_y2         INTEGER NOT NULL DEFAULT 0,
    source_type     TEXT NOT NULL DEFAULT 'MachineLearning',

    FOREIGN KEY (asset_id) REFERENCES media(id) ON DELETE CASCADE,
    FOREIGN KEY (person_id) REFERENCES people(id) ON DELETE SET NULL
);

CREATE INDEX idx_asset_faces_asset ON asset_faces(asset_id);
CREATE INDEX idx_asset_faces_person ON asset_faces(person_id);
```

The `face_count` column on `people` is a denormalised count maintained by triggers or updated during sync. It enables sorting the People sidebar by number of photos without a join.

## Library Trait

### `LibraryFaces` (`src/library/faces.rs`) — [#179](https://github.com/justinf555/Moments/issues/179)

```rust
use crate::library::media::MediaId;

/// Unique identifier for a person (Immich UUID).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PersonId(String);

/// A recognised person with face detection data.
#[derive(Debug, Clone)]
pub struct Person {
    pub id: PersonId,
    pub name: String,
    pub face_count: u32,
    pub is_hidden: bool,
}

#[async_trait::async_trait]
pub trait LibraryFaces {
    /// List all people, ordered by face count descending.
    /// If `include_hidden` is false, hidden people are excluded.
    /// If `include_unnamed` is false, people with empty names are excluded.
    async fn list_people(
        &self,
        include_hidden: bool,
        include_unnamed: bool,
    ) -> Result<Vec<Person>, LibraryError>;

    /// Get media IDs for all assets containing a specific person.
    async fn list_media_for_person(
        &self,
        person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError>;

    /// Rename a person (Immich API + local cache update).
    async fn rename_person(
        &self,
        person_id: &PersonId,
        name: &str,
    ) -> Result<(), LibraryError>;

    /// Hide or unhide a person.
    async fn set_person_hidden(
        &self,
        person_id: &PersonId,
        hidden: bool,
    ) -> Result<(), LibraryError>;

    /// Merge source people into the target person.
    async fn merge_people(
        &self,
        target: &PersonId,
        sources: &[PersonId],
    ) -> Result<(), LibraryError>;
}
```

### Integration with `Library` supertrait

`Library` gains `LibraryFaces` as an additional sub-trait:

```rust
pub trait Library:
    LibraryStorage + LibraryImport + LibraryMedia
    + LibraryThumbnail + LibraryViewer + LibraryAlbums
    + LibraryFaces + Send + Sync
{}
```

### Backend implementations

| Backend | Implementation |
|---------|---------------|
| `ImmichLibrary` | Reads from local `people` / `asset_faces` tables; writes via Immich API then updates cache |
| `LocalLibrary` | Returns empty `Vec` / `Err(NotSupported)` for all methods |

## Sync Integration — [#180](https://github.com/justinf555/Moments/issues/180)

### Sync stream changes

Add `"PeopleV1"` and `"AssetFacesV1"` to the `types` array in `SyncStreamRequest`:

```rust
let request = SyncStreamRequest {
    types: vec![
        "AssetsV1".to_string(),
        "AssetExifsV1".to_string(),
        "AlbumsV1".to_string(),
        "AlbumToAssetsV1".to_string(),
        "PeopleV1".to_string(),        // NEW
        "AssetFacesV1".to_string(),     // NEW
    ],
};
```

### New sync handlers

| Entity Type | Handler | DB Operation |
|-------------|---------|-------------|
| `PersonV1` | `handle_person` | Upsert into `people` table |
| `PersonDeleteV1` | `handle_person_delete` | Delete from `people` table |
| `AssetFaceV1` | `handle_asset_face` | Upsert into `asset_faces`, update `face_count` |
| `AssetFaceDeleteV1` | `handle_asset_face_delete` | Delete from `asset_faces`, update `face_count` |

### Face count maintenance

After each `AssetFaceV1` or `AssetFaceDeleteV1`, update the `face_count` on the affected person:

```sql
UPDATE people SET face_count = (
    SELECT COUNT(*) FROM asset_faces WHERE person_id = ?
) WHERE id = ?;
```

### Person thumbnail downloads

Reuse the existing `ThumbnailDownloader` pattern. After syncing a `PersonV1` record, queue a download if the person's face thumbnail doesn't exist locally:

```
{thumbnails_dir}/people/{person_id}.jpg
```

Downloaded via `GET /people/{id}/thumbnail`. These are 250×250 JPEG crops — small enough that no resizing is needed.

### Sync reset handling

On `SyncResetV1`, clear the `people` and `asset_faces` tables alongside the existing media/album reset logic.

## Database Queries — [#181](https://github.com/justinf555/Moments/issues/181)

New file: `src/library/db/faces.rs`

```rust
impl Database {
    pub async fn upsert_person(&self, ...) -> Result<()>;
    pub async fn delete_person(&self, id: &str) -> Result<()>;
    pub async fn list_people(&self, include_hidden: bool, include_unnamed: bool) -> Result<Vec<PersonRow>>;
    pub async fn upsert_asset_face(&self, ...) -> Result<()>;
    pub async fn delete_asset_face(&self, id: &str) -> Result<()>;
    pub async fn list_media_for_person(&self, person_id: &str) -> Result<Vec<String>>;
    pub async fn update_face_count(&self, person_id: &str) -> Result<()>;
    pub async fn rename_person(&self, id: &str, name: &str) -> Result<()>;
    pub async fn set_person_hidden(&self, id: &str, hidden: bool) -> Result<()>;
    pub async fn clear_people(&self) -> Result<()>;
    pub async fn clear_asset_faces(&self) -> Result<()>;
}
```

## MediaFilter Extension — [#182](https://github.com/justinf555/Moments/issues/182)

Add a `Person` variant to `MediaFilter`:

```rust
pub enum MediaFilter {
    All,
    Favorites,
    Trashed,
    RecentImports { since: i64 },
    Album { album_id: AlbumId },
    Person { person_id: PersonId },  // NEW
}
```

This allows `PhotoGridModel` to load media filtered by person — the grid view infrastructure already handles arbitrary filters. The SQL query for `Person` filter:

```sql
SELECT m.* FROM media m
INNER JOIN asset_faces af ON af.asset_id = m.id
WHERE af.person_id = ?
AND m.is_trashed = 0
ORDER BY m.taken_at DESC, m.imported_at DESC
```

## UI: People Sidebar Section — [#183](https://github.com/justinf555/Moments/issues/183)

### Sidebar changes

Add a "People" section between the static routes and albums:

```
┌─────────────────────┐
│  📷 Photos          │
│  ⭐ Favorites       │
│  📥 Recent Imports  │
│  🗑️ Trash           │
│                     │
│  ── People ──       │
│  👤 Alice (342)     │
│  👤 Bob (128)       │
│  👤 Unnamed (45)    │
│  ... show more      │
│                     │
│  ── Albums ──       │
│  📁 Holiday 2024    │
│  📁 Family          │
└─────────────────────┘
```

Each person row shows:
- Face thumbnail (circular, 32px)
- Name (or "Unnamed" for empty names)
- Face count badge

Initially show the top N people (sorted by face count). A "Show more" expander or scrollable list reveals all people.

### Person grid view

Clicking a person in the sidebar navigates to a `PhotoGridView` with `MediaFilter::Person { person_id }`. This uses the existing grid infrastructure — no new view widget needed. The dynamic route registration pattern from albums applies here:

```rust
if let Some(person_id_str) = id.strip_prefix("person:") {
    // Register lazily, same as album views
    let person_id = PersonId::from_raw(person_id_str.to_owned());
    let model = PhotoGridModel::new(lib, tk, MediaFilter::Person { person_id });
    // ... same pattern as album views
}
```

## UI: Face Overlay in Viewer — [#184](https://github.com/justinf555/Moments/issues/184)

Optional enhancement: when viewing a photo in the `PhotoViewer`, draw face bounding boxes as overlay rectangles. Clicking a box navigates to that person's grid view.

This uses the `asset_faces` bounding box data already synced. Implementation is a GTK `DrawingArea` overlay on the viewer's `GtkPicture`.

This is a **nice-to-have** and not required for the initial integration.

## UI: Person Management — [#185](https://github.com/justinf555/Moments/issues/185)

Context menu on a person row in the sidebar:

| Action | API | Description |
|--------|-----|-------------|
| Rename | `PUT /people/{id}` | Edit the display name |
| Hide | `PUT /people/{id}` | Toggle `isHidden`, removes from sidebar |
| Merge | `POST /people/{id}/merge` | Combine two people |

These are write-through operations: API call first, then update local cache on success.

## Implementation Phases

| Phase | Issue | Description | Depends On |
|-------|-------|-------------|------------|
| 1 | [#179](https://github.com/justinf555/Moments/issues/179) | `LibraryFaces` trait + `PersonId` type + stub implementations | — |
| 2 | [#181](https://github.com/justinf555/Moments/issues/181) | DB migrations (012, 013) + `db/faces.rs` queries | #179 |
| 3 | [#180](https://github.com/justinf555/Moments/issues/180) | Sync stream: `PeopleV1` + `AssetFacesV1` handlers + person thumbnail downloads | #181 |
| 4 | [#182](https://github.com/justinf555/Moments/issues/182) | `MediaFilter::Person` variant + SQL query | #181 |
| 5 | [#183](https://github.com/justinf555/Moments/issues/183) | Sidebar "People" section + person grid view routing | #180, #182 |
| 6 | [#185](https://github.com/justinf555/Moments/issues/185) | Person management (rename, hide, merge) | #183 |
| 7 | [#184](https://github.com/justinf555/Moments/issues/184) | Face bounding box overlay in viewer (optional) | #180 |

Phases 1–3 are backend-only and can be validated via `cargo test` + sync logs.
Phases 4–5 deliver the visible feature.
Phase 6 adds management capabilities.
Phase 7 is a polish enhancement.

## Edge Cases

- **Unnamed people**: Immich clusters faces before users name them. Show as "Unnamed" with face thumbnail. Allow filtering these out via `include_unnamed`.
- **Hidden people**: Respect the `isHidden` flag from Immich. Don't show in sidebar by default.
- **No faces detected**: Some assets have no face data. The People section simply shows fewer results.
- **Person with no assets**: Can occur after face reassignment. Show in sidebar with count 0; grid view shows empty state.
- **Local backend**: All `LibraryFaces` methods return empty results. The sidebar People section is hidden when using the local backend.
- **Large face counts**: Users with 50+ people — sidebar should be scrollable or collapsible, not an unbounded list.
- **Sync ordering**: `AssetFaceV1` records reference `person_id` which may arrive before the corresponding `PersonV1`. Use `ON DELETE SET NULL` foreign key and tolerate NULL `person_id` values.

## Testing

- Unit tests for all `db/faces.rs` queries (upsert, delete, list, filter)
- Unit tests for `PersonId` newtype
- Integration validation: compare `people` table count with Immich `/people` endpoint total
- Manual testing: sidebar shows people, clicking navigates to person grid, thumbnails load

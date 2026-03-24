# Immich Backend Design

## Overview

The Immich backend connects Moments to a self-hosted [Immich](https://immich.app) server, providing a native GNOME desktop client for browsing, managing, and uploading photos stored on Immich.

## Core Principle: Offline-First

The Immich backend works fully offline after initial sync. All data — assets, albums, album memberships, metadata, thumbnails — is cached in the same local SQLite schema used by the local backend. The UI queries the local database, never the Immich API directly.

```
┌──────────────┐   AssetSynced / ThumbnailReady   ┌──────────────────┐
│  GTK UI      │ ◄──────────────────────────────── │  SyncManager     │
│  (grid,      │                                   │  (background     │
│   sidebar,   │     reads from local DB           │   Tokio task,    │
│   viewer)    │ ──────────────────────────► ┌─────┤   polls every    │
└──────────────┘                             │     │   30 seconds)    │
                                             │     └────────┬─────────┘
                                             │  Database    │
                                             │  (SQLite)    │ ThumbnailDownloader
                                             │  same schema │ (4 concurrent workers)
                                             │  as local    │        │
                                             └──────┬───────┘        │
                                                    │ syncs to/from  │
                                                    ▼                ▼
                                             ┌──────────────┐
                                             │ ImmichClient  │
                                             │ (HTTP/REST)   │
                                             │ reqwest +     │
                                             │ Bearer token  │
                                             └──────┬────────┘
                                                    │
                                                    ▼
                                             ┌──────────────┐
                                             │ Immich Server │
                                             └──────────────┘
```

## Architecture

### ImmichLibrary Provider

`src/library/providers/immich.rs` — implements all Library sub-traits:

| Trait | Read Path | Write Path | Status |
|-------|-----------|------------|--------|
| `LibraryStorage` | Open local cache DB | Close, stop sync | ✅ Done |
| `LibraryMedia` | `self.db.list_media(...)` | API call → update local cache | ✅ Done |
| `LibraryAlbums` | `self.db.list_albums(...)` | Stubs (API not wired yet) | 🔜 #105 |
| `LibraryThumbnail` | Local file path (cached) | Downloaded by ThumbnailDownloader | ✅ Done |
| `LibraryViewer` | On-demand download with LRU disk cache | N/A | ✅ Done |
| `LibraryImport` | N/A | Upload via `POST /assets` | 🔜 #106 |

All reads delegate to `self.db` — identical SQL to `LocalLibrary`. Writes go to the Immich API first, then update the local cache to match.

### ImmichClient

`src/library/immich_client.rs` — HTTP client wrapping `reqwest`:

- Session-based authentication via `Authorization: Bearer {token}`
- `login()` static method for email/password authentication
- Session token stored in GNOME Keyring via `libsecret` (never plain text on disk)
- Server URL stored in the bundle manifest (`library.toml`)
- Generic helpers: `get`, `post`, `put`, `delete`, `get_bytes`, `post_stream`, `put_no_content`, `delete_with_body`, `post_no_content`
- `validate()` for connection testing (used by setup wizard)

### SyncManager

`src/library/sync.rs` — persistent background service:

**Responsibilities:**
1. **Initial sync** on library open — streams all assets and EXIF data
2. **Periodic polling** every 30 seconds — delta sync to pick up changes from mobile uploads and other clients
3. **Thumbnail download** — bounded worker pool (4 concurrent) downloads thumbnails to local disk
4. **Per-asset event emission** — fires `AssetSynced` events for incremental grid updates (no full reload)

**Sync Protocol:**
- Uses Immich's `POST /sync/stream` (newline-delimited JSON) with entity types: `AssetsV1`, `AssetExifsV1`
- Match-based dispatch for entity types (AssetV1, AssetExifV1, AssetDeleteV1, SyncCompleteV1, SyncResetV1)
- Tracks sync checkpoints via `POST /sync/ack`, persisted locally in `sync_checkpoints` table
- `INSERT OR REPLACE` for all upserts — no pre-check queries needed
- Transient errors don't abort the polling loop — logged and retried next cycle

**Reset Sync (>30 days stale):**
When the server sends `SyncResetV1`, we do NOT wipe the local cache. Instead:
1. Load all existing MediaIds into a `HashSet<String>` (~24 MB for 200k library)
2. Process the full stream normally — `INSERT OR REPLACE` handles create/update
3. Remove each seen ID from the HashSet
4. After `SyncCompleteV1`: batch delete anything remaining (orphaned entries)

This preserves existing cached data and thumbnails — no visible disruption to the user.

**Thumbnail Download Worker Pool:**
```
SyncManager: AssetV1 arrives
  → INSERT OR REPLACE into media table
  → emit AssetSynced event (incremental grid update)
  → db.insert_thumbnail_pending(id)
  → tx.send(media_id) onto bounded channel (capacity 1000)
  → ThumbnailDownloader worker picks it up (max 4 concurrent via Semaphore)
  → GET /assets/{id}/thumbnail?size=thumbnail (250px)
  → write to thumbnails/{shard1}/{shard2}/{id}.webp
  → db.set_thumbnail_ready(id)
  → emit ThumbnailReady event
  → grid cell repaints (if visible)
```

Skips download if the thumbnail file already exists on disk (efficient for reset sync).

**Event Flow (Incremental — No Scroll Jump):**
```
SyncManager: AssetV1 arrives
  → emit LibraryEvent::AssetSynced { item: MediaItem }
  → GTK idle loop picks it up
  → ModelRegistry::on_asset_synced(item)
      → for each model: if filter.matches(item) → insert_item_sorted()
  → item appears at correct sorted position without clearing the store
  → scroll position preserved
```

**Lifecycle:**
- Started on `ImmichLibrary::open()`
- Runs as a Tokio task: sync → sleep 30s → sync → sleep 30s → ...
- Shutdown signal via `tokio::sync::watch` channel interrupts sleep immediately
- Stopped on `ImmichLibrary::close()`

### Media Write-Through

`src/library/providers/immich.rs` — write operations call the Immich API first, then update the local cache:

| Action | API Call | Local Cache |
|--------|----------|-------------|
| Favorite | `PUT /assets { ids, isFavorite }` | `db.set_favorite()` |
| Trash | `DELETE /assets { ids }` | `db.trash()` |
| Restore | `POST /trash/restore/assets { ids }` | `db.restore()` |
| Delete permanently | `DELETE /assets { ids, force: true }` | `db.delete_permanently()` |

If the API call fails, the local cache is NOT updated — consistent state guaranteed.

### Original File Viewer

`src/library/providers/immich.rs` — `LibraryViewer::original_path()`:

- **Cache hit**: return local path instantly (works offline)
- **Cache miss**: `GET /assets/{id}/original` → write to `originals/{shard1}/{shard2}/{id}.{ext}` → return path
- File extension from `original_filename` in DB (needed by image decoder)
- **LRU eviction** on library open: walks cache dir, sorts by mtime, deletes oldest until under configured limit
- **GSettings**: `originals-cache-max-mb` (default 2048 = 2 GB, 0 = no eviction)
- Thumbnails are never evicted — only originals

### Incremental Grid Updates

`src/ui/photo_grid/model.rs` — all grid updates preserve scroll position:

| Action | Mechanism |
|--------|-----------|
| Sync: new asset | `AssetSynced` event → `insert_item_sorted()` |
| Favorite toggle (All view) | Update property in place |
| Favorite added (Favorites view) | `fetch_and_insert_sorted()` |
| Unfavorite (Favorites view) | `remove_item()` |
| Trash (from any view) | `remove_item()` |
| Restore (to any view) | `fetch_and_insert_sorted()` |
| Delete permanently | `remove_item()` |
| Thumbnail ready | `set_texture()` on existing item |

Key building blocks:
- `MediaFilter::matches(item)` — pure in-memory filter evaluation
- `PhotoGridModel::insert_item_sorted(item)` — binary search for correct descending position
- `PhotoGridModel::fetch_and_insert_sorted(id)` — async DB fetch + sorted insert
- `LibraryMedia::get_media_item(id)` — single SELECT by PK

Album media changes still use `reload()` (filter requires DB query for membership check).

### GPU Memory Management

- **Texture loading**: only in factory `bind` callback (when cell becomes visible)
- **Texture eviction**: factory `unbind` callback sets `texture = None` (when cell scrolls off-screen)
- **No speculative loading**: textures never created for off-screen items
- **Result**: GPU VRAM bounded to `visible_cells × 250KB ≈ 12-25 MB` instead of `total_assets × 250KB`
- Grid cells show a static `image-x-generic-symbolic` placeholder instead of animated spinners (zero CPU cost)

### Local Cache

The Immich backend reuses the same `Database` struct and SQLite schema as the local backend:

- `media` table — cached asset records (using Immich UUIDs as MediaId)
- `media_metadata` table — cached EXIF data
- `albums` table — cached album records (🔜 #105)
- `album_media` table — cached album memberships (🔜 #105)
- `thumbnails` table — thumbnail status tracking
- `sync_checkpoints` table — ack strings per entity type for delta sync

This means all existing queries, filters (`All`, `Favorites`, `Trashed`, `RecentImports`, `Album`), and pagination work unchanged.

**Bundle structure:**
```
Moments-Immich.library/
├── library.toml        # [library] backend="immich" + [immich] server_url
├── thumbnails/         # Cached thumbnails (sharded: {hex[..2]}/{hex[2..4]}/{id}.webp)
├── originals/          # On-demand cached originals (LRU eviction, sharded with extension)
├── database/
│   └── moments.db      # Local SQLite cache (same schema as local backend)
```

### ID Mapping

Immich uses UUIDs for asset IDs. We use these directly as `MediaId` values in the local cache — no hash-based content addressing for Immich assets. The `MediaId` newtype treats both as opaque strings.

For uploads from Moments → Immich (🔜 #106), we'll compute SHA-1 (Immich's dedup hash) alongside BLAKE3 (our content ID).

## Authentication

Immich uses **session-based auth** — the `POST /sync/stream` endpoint rejects API keys and requires a session token.

**Login flow:**
1. Setup wizard collects server URL + email + password
2. `POST /api/auth/login` with `{ email, password }` → returns `{ accessToken, userId, name }`
3. `accessToken` is a persistent session token (no expiry by default)
4. Stored in GNOME Keyring via `libsecret` (keyed by `server_url`)
5. `ImmichClient` uses `Authorization: Bearer {token}` on all requests

**Session lifecycle:**
- Sessions persist indefinitely until: password change, explicit logout, or admin revocation
- On 401 response: prompt user to re-authenticate (future enhancement)
- No refresh token — the access token IS the session

**Why not API keys:**
- `POST /sync/stream` explicitly rejects API keys
- Session auth is what the Immich mobile app uses
- Future-proof as Immich deprecates API-key-compatible endpoints

**Credential storage** via GNOME Keyring (`libsecret`):
- Schema: `io.github.justinf555.Moments` with attribute `server_url`
- Each Immich server gets its own keyring entry
- Stores the session token, never the password
- Never written to disk in plain text
- Requires Flatpak permission: `--talk-name=org.freedesktop.secrets`

Module: `src/library/keyring.rs`

## Sync Protocol

The sync engine uses `POST /sync/stream` which returns **newline-delimited JSON**
(content-type `application/jsonlines+json`). Each line is:

```json
{"type":"AssetV1","data":{...},"ack":"AssetV1|019513a2-..."}
```

The `ack` field is sent back via `POST /sync/ack` to checkpoint progress. The
server tracks checkpoints per-session and only sends changes since the last
acknowledged position on subsequent syncs.

**Stream lifecycle:**
1. First sync (no checkpoints): server streams all data
2. Last line is always `SyncCompleteV1` — ack this to mark sync complete
3. Subsequent syncs: only changes since last checkpoint
4. If checkpoint is >30 days old: server sends `SyncResetV1` — handled via HashSet-based reconciliation (no data loss)

### Sync Request Types

We subscribe to these types via the `types` array in the request:

| Request Type | Entity Types Produced | What Changes |
|-------------|----------------------|-------------|
| `AssetsV1` | `AssetV1`, `AssetDeleteV1` | Asset created/updated/deleted |
| `AssetExifsV1` | `AssetExifV1` | EXIF metadata changes |
| `AlbumsV1` | `AlbumV1`, `AlbumDeleteV1` | Album created/updated/deleted (🔜 #105) |
| `AlbumToAssetsV1` | `AlbumToAssetV1`, `AlbumToAssetDeleteV1` | Assets added/removed from albums (🔜 #105) |

Other types (People, Faces, Memories, Partners, Stacks) can be added later.

## API Endpoints Used

### Auth
| Endpoint | Purpose | Status |
|----------|---------|--------|
| `POST /auth/login` | Login with email/password → session token | ✅ |

### Sync
| Endpoint | Purpose | Status |
|----------|---------|--------|
| `POST /sync/stream` | Stream changes as newline-delimited JSON | ✅ |
| `POST /sync/ack` | Acknowledge processed changes | ✅ |

### Assets
| Endpoint | Purpose | Status |
|----------|---------|--------|
| `GET /assets/{id}/thumbnail` | Download thumbnail (250px) | ✅ |
| `GET /assets/{id}/original` | Download original file | ✅ |
| `PUT /assets` | Update asset (favorite) | ✅ |
| `DELETE /assets` | Trash / permanently delete | ✅ |
| `POST /assets` | Upload new asset (multipart) | 🔜 #106 |
| `POST /assets/bulk-upload-check` | Dedup check before upload | 🔜 #106 |

### Albums
| Endpoint | Purpose | Status |
|----------|---------|--------|
| `GET /albums` | List all albums | 🔜 #105 |
| `POST /albums` | Create album | 🔜 #105 |
| `PATCH /albums/{id}` | Rename album | 🔜 #105 |
| `DELETE /albums/{id}` | Delete album | 🔜 #105 |
| `PUT /albums/{id}/assets` | Add assets to album | 🔜 #105 |
| `DELETE /albums/{id}/assets` | Remove assets from album | 🔜 #105 |

### Trash
| Endpoint | Purpose | Status |
|----------|---------|--------|
| `POST /trash/restore/assets` | Restore trashed assets | ✅ |

### Server
| Endpoint | Purpose | Status |
|----------|---------|--------|
| `GET /server/ping` | Connection check | ✅ |
| `GET /server/about` | Server version info | ✅ |

## Configuration

GSettings keys:
- `library-path` — path to the Immich library bundle
- `originals-cache-max-mb` — max disk cache for downloaded originals (default 2048 MB)
- `recent-imports-days` — days to show in Recent Imports view (default 30)
- `zoom-level` — grid thumbnail size
- `window-width`, `window-height`, `is-maximized` — window geometry

Planned:
- `sync-interval-seconds` — polling interval for delta sync (currently hardcoded 30s, #119)

## Implementation Status

| Issue | Description | Status |
|-------|-------------|--------|
| #101 | HTTP client & authentication | ✅ Merged |
| #108 | Setup wizard (server connection UI) | ✅ Merged |
| #114 | Session-based auth (replaces API key) | ✅ Merged |
| #102 | ImmichLibrary provider (LibraryStorage) | ✅ Merged |
| #109 | Background sync engine (SyncManager) | ✅ Merged |
| #104 | Thumbnail download (worker pool) | ✅ Merged |
| #103 | Media write-through (favorites/trash) | ✅ Merged |
| #107 | Original file viewer (LRU disk cache) | ✅ Merged |
| #120 | Incremental grid updates (no scroll jump) | ✅ Merged |
| #65 | GPU texture eviction | ✅ Merged |
| #125 | Static placeholder icons | ✅ Merged |
| — | Periodic polling (30s) | ✅ Merged |
| #105 | Album sync | 🔜 Next |
| #106 | Upload from Moments to Immich | 🔜 |
| #119 | Configurable sync interval | 🔜 |
| #127 | Debounce texture loading during scroll | 🔜 |

## Future Enhancements

- **Offline write queue** — queue writes when offline, sync when reconnected
- **Video streaming** — pass HTTP URL to GStreamer instead of downloading full file first
- **Conflict resolution** — currently server wins; could add merge strategies
- **Search** — proxy Immich's smart search / CLIP search
- **People/Faces** — sync face data and display in UI
- **Shared albums** — Immich supports multi-user album sharing
- **Map view** — GPS data is already synced in metadata
- **401 re-authentication** — prompt user when session token is revoked

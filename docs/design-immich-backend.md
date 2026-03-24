# Immich Backend Design

## Overview

The Immich backend connects Moments to a self-hosted [Immich](https://immich.app) server, providing a native GNOME desktop client for browsing, managing, and uploading photos stored on Immich.

## Core Principle: Offline-First

The Immich backend works fully offline after initial sync. All data — assets, albums, album memberships, metadata, thumbnails — is cached in the same local SQLite schema used by the local backend. The UI queries the local database, never the Immich API directly.

```
┌──────────────┐     LibraryEvent channel     ┌──────────────┐
│  GTK UI      │ ◄──────────────────────────── │ SyncManager  │
│  (grid,      │                               │ (background  │
│   sidebar,   │     reads from local DB       │  Tokio task) │
│   viewer)    │ ──────────────────────────► ┌─┴──────────────┤
└──────────────┘                             │   Database     │
                                             │   (SQLite)     │
                                             │   same schema  │
                                             │   as local     │
                                             │   backend      │
                                             └─┬──────────────┘
                                               │ syncs to/from
                                               ▼
                                        ┌──────────────┐
                                        │ ImmichClient  │
                                        │ (HTTP/REST)   │
                                        │ reqwest +     │
                                        │ x-api-key     │
                                        └──────────────┘
                                               │
                                               ▼
                                        ┌──────────────┐
                                        │ Immich Server │
                                        └──────────────┘
```

## Architecture

### ImmichLibrary Provider

`src/library/providers/immich.rs` — implements all Library sub-traits:

| Trait | Read Path | Write Path |
|-------|-----------|------------|
| `LibraryStorage` | Open local cache DB | Close, stop sync |
| `LibraryMedia` | `self.db.list_media(...)` | API call → update local cache |
| `LibraryAlbums` | `self.db.list_albums(...)` | API call → update local cache |
| `LibraryThumbnail` | Local file path (cached) | Downloaded by SyncManager |
| `LibraryViewer` | Local cache or on-demand download | N/A |
| `LibraryImport` | N/A | Upload via `POST /assets` |

All reads delegate to `self.db` — identical SQL to `LocalLibrary`. Writes go to the Immich API first, then update the local cache to match.

### ImmichClient

`src/library/immich_client.rs` — HTTP client wrapping `reqwest`:

- Authentication via `x-api-key` header
- API key stored in GNOME Keyring via `libsecret` (never plain text on disk)
- Server URL stored in the bundle manifest (`library.toml`)
- Generic `get/post/put/delete` helpers
- `validate()` for connection testing (used by setup wizard)

### SyncManager

`src/library/sync.rs` — persistent background service:

**Responsibilities:**
1. **Initial sync** on library open — full sync of all assets, albums, metadata
2. **Periodic polling** (configurable interval, default 30s) — delta sync to pick up changes from mobile uploads and other clients
3. **Thumbnail pre-fetch** — download thumbnails to local disk for instant browsing
4. **Event emission** — fires `LibraryEvent`s through the existing channel so the UI updates live

**Sync Protocol:**
- Uses Immich's `POST /sync/delta-sync` with entity types: `AssetsV1`, `AlbumsV1`, `AlbumToAssetsV1`, `AssetExifV1`
- Tracks last sync checkpoint via `POST /sync/ack`
- For each new/updated asset: upsert into local `media` + `media_metadata` tables
- For each new thumbnail: download via `GET /assets/{id}/thumbnail` → write to sharded `thumbnails/` dir
- For deleted assets: remove from local DB + delete cached thumbnail

**Event Flow:**
```
SyncManager detects new asset
  → inserts into local DB
  → downloads thumbnail to thumbnails/{shard}/{id}.webp
  → sends LibraryEvent::ThumbnailReady { media_id }
  → GTK idle loop picks it up
  → ModelRegistry broadcasts to all grid models
  → grid cell repaints with new thumbnail

SyncManager detects batch complete
  → sends LibraryEvent::ImportComplete or custom SyncComplete
  → registry.reload_all() refreshes all views
```

**Lifecycle:**
- Started on `ImmichLibrary::open()`
- Runs as a Tokio task in the background
- Stopped on `ImmichLibrary::close()`
- Gracefully handles offline/unreachable server (uses cached data, retries on next interval)

### Local Cache

The Immich backend reuses the same `Database` struct and SQLite schema as the local backend:

- `media` table — cached asset records (using Immich UUIDs as MediaId)
- `media_metadata` table — cached EXIF data
- `albums` table — cached album records
- `album_media` table — cached album memberships
- `thumbnails` table — thumbnail status tracking

This means all existing queries, filters (`All`, `Favorites`, `Trashed`, `RecentImports`, `Album`), and pagination work unchanged.

**Bundle structure:**
```
Moments-Immich.library/
├── library.toml        # [library] backend="immich" + [immich] server_url
├── thumbnails/         # Cached WebP thumbnails (sharded)
├── originals_cache/    # On-demand cached originals (LRU eviction)
└── library.db          # Local SQLite cache
```

### ID Mapping

Immich uses UUIDs for asset IDs. We use these directly as `MediaId` values in the local cache — no hash-based content addressing for Immich assets. The `MediaId` newtype already accepts arbitrary strings.

For uploads from Moments → Immich, we compute SHA-1 (Immich's dedup hash) alongside BLAKE3 (our content ID).

## API Endpoints Used

### Sync
| Endpoint | Purpose |
|----------|---------|
| `POST /sync/delta-sync` | Incremental changes since last checkpoint |
| `POST /sync/full-sync` | Full initial sync |
| `POST /sync/ack` | Acknowledge processed changes |

### Assets
| Endpoint | Purpose |
|----------|---------|
| `GET /assets/{id}` | Asset details and metadata |
| `GET /assets/{id}/thumbnail` | Download thumbnail |
| `GET /assets/{id}/original` | Download original file |
| `PUT /assets` | Update asset (favorite, etc.) |
| `DELETE /assets` | Trash assets |
| `POST /assets` | Upload new asset (multipart) |
| `POST /assets/bulk-upload-check` | Dedup check before upload |

### Albums
| Endpoint | Purpose |
|----------|---------|
| `GET /albums` | List all albums |
| `POST /albums` | Create album |
| `PATCH /albums/{id}` | Rename album |
| `DELETE /albums/{id}` | Delete album |
| `PUT /albums/{id}/assets` | Add assets to album |
| `DELETE /albums/{id}/assets` | Remove assets from album |

### Trash
| Endpoint | Purpose |
|----------|---------|
| `POST /trash/restore/assets` | Restore trashed assets |
| `POST /trash/empty` | Empty trash |

### Server
| Endpoint | Purpose |
|----------|---------|
| `GET /server/ping` | Connection check |
| `GET /server/about` | Server version info |

## Credential Storage

API keys are stored in the GNOME Keyring via `libsecret`:
- Schema: `io.github.justinf555.Moments` with attribute `server_url`
- Each Immich server gets its own keyring entry
- Never written to disk in plain text
- Requires Flatpak permission: `--talk-name=org.freedesktop.secrets`

Module: `src/library/keyring.rs`

## Sync Entity Types

The delta sync API reports changes across these categories:

| Entity Type | What Changes |
|-------------|-------------|
| `AssetsV1` / `AssetDeleteV1` | Asset created/updated/deleted |
| `AssetExifV1` | EXIF metadata changes |
| `AlbumsV1` | Album created/updated/deleted |
| `AlbumToAssetV1` | Assets added/removed from albums |

We subscribe to these four types. Other types (People, Faces, Memories, Partners, Stacks) can be added later as we implement those features.

## Configuration

GSettings keys:
- `library-path` — path to the Immich library bundle (existing)
- `sync-interval-seconds` — polling interval for delta sync (default 30)

## Implementation Order

1. ~~#101 — HTTP client & authentication~~ ✓
2. ~~#108 — Setup wizard (server connection UI)~~ ✓
3. #102 — ImmichLibrary provider (LibraryStorage)
4. #109 — Background sync engine (SyncManager)
5. #103 — LibraryMedia impl (reads from cache, writes to API)
6. #104 — LibraryThumbnail (pre-fetch & cache)
7. #105 — LibraryAlbums (cached album operations)
8. #106 — LibraryImport (upload to server)
9. #107 — LibraryViewer (original file cache)

## Future Enhancements

- **Offline write queue** — queue writes when offline, sync when reconnected
- **Conflict resolution** — currently server wins; could add merge strategies
- **Search** — proxy Immich's smart search / CLIP search
- **People/Faces** — sync face data and display in UI
- **Shared albums** — Immich supports multi-user album sharing
- **Map view** — GPS data is already synced in metadata

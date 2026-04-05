# Architecture

This document describes the architecture of Moments as of April 2026. It is intended for engineers joining the project — read this before diving into the code.

## Overview

Moments is a GNOME photo management app written in Rust, targeting GNOME Circle. It uses GTK4 + libadwaita for the UI, SQLite for persistence, and a trait-based library abstraction with two backends (local filesystem and Immich server).

The app has two async executors that must never be confused:

- **GTK executor** (`glib::MainContext`) — UI thread only. Widget updates, signal handlers, property bindings.
- **Tokio executor** — all backend I/O: database queries, file operations, image decoding, HTTP calls to Immich.

Results flow from Tokio → GTK via `Sender<LibraryEvent>` (a `std::sync::mpsc` channel which is `Send`). The idle loop in `application.rs` translates `LibraryEvent` → `AppEvent` and sends via the event bus for fan-out delivery to all subscribers.

## Module Map

```
src/
  main.rs              Entry point: tracing, gettext, GResources, Tokio runtime, app.run()
  application.rs       MomentsApplication — lifecycle, GActions, event polling, error toasts
  config.rs            Compile-time constants (VERSION, PKGDATADIR, etc.)

  library.rs           Library supertrait (blanket impl of all feature sub-traits)
  library/
    storage.rs         LibraryStorage — open/close lifecycle, set_sync_interval, set_cache_limit
    import.rs          LibraryImport — batch folder import
    media.rs           LibraryMedia — CRUD + queries; MediaId, MediaItem, MediaFilter, MediaCursor
    album.rs           AlbumId, Album, LibraryAlbums trait
    faces.rs           PersonId, Person, LibraryFaces trait
    editing.rs         EditState types, LibraryEditing trait (non-destructive editing)
    edit_renderer.rs   apply_edits() pure function (exposure, color, transforms, filters)
    thumbnail.rs       LibraryThumbnail — thumbnail path resolution + DB state
    viewer.rs          LibraryViewer — resolve original file path
    error.rs           LibraryError (thiserror)
    event.rs           LibraryEvent enum (Ready, ThumbnailReady, SyncComplete, etc.)
    config.rs          LibraryConfig (Local | Immich)
    bundle.rs          On-disk library structure (originals/, thumbnails/, database/)
    factory.rs         LibraryFactory — constructs concrete backends, returns Arc<dyn Library>
    db.rs              Database (sqlx::SqlitePool)
    db/
      media.rs         LibraryMedia impl — read queries, filter_clause, sort_expr
      media_write.rs   Insert, favourite, trash, restore, delete
      albums.rs        LibraryAlbums impl
      edits.rs         Edit state CRUD (get/upsert/delete/mark_rendered)
      faces.rs         People/face CRUD (upsert, list, face_count maintenance)
      sync.rs          Sync upserts, checkpoints, audit methods
      thumbnails.rs    Thumbnail status tracking
      stats.rs         Aggregate library statistics query
      upload.rs        Upload queue CRUD
      migrations/      Numbered SQL migrations (001–014)
    exif.rs            EXIF metadata extraction (kamadak-exif)
    format/            Format detection (magic bytes, standard/raw/video handlers)
    importer.rs        Local import job (walk_dir, collect_candidates)
    immich_importer.rs ImmichImportJob (upload to Immich server)
    immich_client.rs   ImmichClient (HTTP client for Immich API)
    keyring.rs         GNOME Keyring integration (session token storage)
    thumbnailer.rs     ThumbnailJob — decode, resize, encode WebP, write atomically
    video_meta.rs      GStreamer-based video duration extraction
    sync.rs            SyncHandle (public API: start, shutdown, set_interval)
    sync/
      manager.rs       SyncManager (sync loop, entity handlers, ack flushing)
      downloader.rs    ThumbnailDownloader worker pool
      types.rs         Immich sync protocol DTOs and parse helpers
      tests.rs         Unit tests for sync manager and handlers
    providers/
      local.rs         LocalLibrary — local filesystem backend
      immich.rs        ImmichLibrary — Immich server backend with offline-first sync

  app_event.rs         AppEvent enum (commands, results, lifecycle events)
  event_bus.rs         EventBus (push-based fan-out delivery via glib::idle_add_once)
  commands/
    dispatcher.rs      CommandDispatcher (routes *Requested events to handlers on Tokio)
    trash.rs           TrashCommand handler
    restore.rs         RestoreCommand handler
    delete.rs          DeleteCommand handler
    favorite.rs        FavoriteCommand handler
    add_to_album.rs    AddToAlbumCommand handler
    remove_from_album.rs RemoveFromAlbumCommand handler
    create_album.rs    CreateAlbumCommand handler

  ui/
    window.rs          MomentsWindow — main shell, wires sidebar, coordinator, views
    sidebar.rs         MomentsSidebar (AdwSidebar) with pinned albums + persistent status bar
    sidebar/
      route.rs         ROUTES definitions (Photos, Favorites, Recent, People, Albums, Trash)
    coordinator.rs     ContentCoordinator — stack-based view routing, returns view_actions
    photo_grid.rs      PhotoGridView (GObject + Blueprint, zoom, selection, viewer integration)
    photo_grid/
      model.rs         PhotoGridModel (GObject, pagination, filtering, incremental updates)
      item.rs          MediaItemObject (GObject wrapper for grid items)
      cell.rs          PhotoGridCell widget (Blueprint template, placeholder → thumbnail → star)
      factory.rs       Cell factory (bind/unbind with texture management + decode semaphore)
      actions.rs       Context menu (per-action handlers), album controls
      action_bar.rs    Selection mode action bar (favourite, album, trash/restore/delete)
      texture_cache.rs LRU cache for decoded RGBA thumbnail pixels
    viewer.rs          PhotoViewer (GObject + Blueprint, adw::NavigationPage subclass)
    viewer/
      loading.rs       Full-res decode, edit session setup, metadata fetching
      menu.rs          Shared overflow menu builder + photo viewer menu wiring
      info_panel.rs    EXIF metadata display panel
      edit_panel.rs    EditPanel (GObject + Blueprint, session mgmt, save/revert, render)
      edit_panel/
        transforms.rs  Rotate/flip buttons
        filters.rs     Filter preset grid + strength slider
        sliders.rs     Adjust sliders (exposure, colour)
    video_viewer.rs    VideoViewer (GObject + Blueprint, GStreamer playback)
    album_grid.rs      AlbumGridView (GObject + Blueprint, sort, empty state, bus subscription)
    album_grid/
      actions.rs       Context menu (open, rename, pin, delete) + drill-down helper
      selection.rs     Enter/exit selection mode, batch delete
      card.rs          AlbumCard widget (Blueprint template, cover mosaic)
      factory.rs       Card factory (bind/unbind with cover thumbnail loading)
      item.rs          AlbumItemObject (GObject wrapper for album items)
    collection_grid.rs CollectionGridView (GObject + Blueprint, reusable grid for People)
    collection_grid/
      actions.rs       Drill-down, context menu (rename, hide/unhide)
      cell.rs          CollectionGridCell widget (Blueprint template, thumbnail + name)
      factory.rs       Cell factory with ThumbnailStyle (Circular/Square)
      item.rs          CollectionItemObject (GObject wrapper for collection items)
    album_dialogs.rs   Create/rename/delete album dialogs
    album_picker_dialog.rs Album picker dialog entry point (async data fetch + present)
    album_picker_dialog/
      dialog.rs        AdwDialog with search, thumbnails, create flow, empty state
      album_row.rs     Album row widget (thumbnail + name + count + checkmark + pill)
      state.rs         AlbumPickerData, AlbumEntry (data-in, events-out)
    import_dialog.rs   Import progress dialog
    preferences_dialog.rs Preferences (sentence case, AdwSpinRow, library stats)
    empty_library.rs   Empty library placeholder view
    setup_window/      Setup wizard (backend picker, local setup, Immich setup)
    widgets.rs         Shared UI components (expander_row, detail_row, section_label)
  style.css            Custom CSS (selection highlight, circular thumbnails, hidden person styling)
```

## Library Abstraction

The GTK layer only sees `Arc<dyn Library>`. It never imports or references concrete backend types.

`Library` is a blanket-impl composition of feature sub-traits:

```
Library = LibraryStorage + LibraryImport + LibraryMedia + LibraryThumbnail
        + LibraryViewer + LibraryAlbums + LibraryFaces + LibraryEditing
```

Each sub-trait is defined in its own file and adds one capability. New features add new sub-traits without modifying existing ones.

`LibraryFactory` is the single place where concrete types are named and constructed. Everything else works through trait objects.

Two backends exist:

- **`LocalLibrary`** (`providers/local.rs`) — stores originals on disk, generates thumbnails locally.
- **`ImmichLibrary`** (`providers/immich.rs`) — syncs with an Immich server via `POST /sync/stream`, caches everything locally in the same SQLite schema. Background sync polls at a configurable interval. Works fully offline; syncs when connected. See `docs/design-immich-backend.md` for the full design.

### MediaId

Every asset is identified by a `MediaId`. The format depends on the backend:

- **Local**: 64-char lowercase hex BLAKE3 hash of the file's raw bytes. Content-addressable — stable across renames and re-imports.
- **Immich**: server-assigned UUID string.

Consumers treat `MediaId` as an opaque string — the grid, viewer, and thumbnail pipeline don't care which format is used. Hashing uses `tokio::task::spawn_blocking` with a streaming hasher so large files are never fully loaded into memory.

### MediaFilter

Queries accept a `MediaFilter` enum (`All` | `Favorites` | `Trashed` | `RecentImports` | `Album(AlbumId)` | `Person(PersonId)`). The filter maps to a SQL WHERE clause fragment via `filter_clause()` in `db/media.rs`. New filters add a variant and a clause — no other changes needed.

### Keyset Pagination

`list_media` uses keyset pagination via `MediaCursor` (last seen `COALESCE(taken_at, 0)` + `id`). This is O(1) per page regardless of library size — no OFFSET scans.

## Database

SQLite via `sqlx` with an async connection pool. The `Database` struct wraps a `SqlitePool` and provides typed CRUD methods split across submodules.

Schema is managed by numbered migrations in `src/library/db/migrations/` (001–014), embedded at compile time via `sqlx::migrate!`. Every schema change must be a new migration file — no ad-hoc DDL in code.

After any schema change, regenerate the offline query snapshot:
```bash
cargo sqlx database create && cargo sqlx migrate run && cargo sqlx prepare
```
CI sets `SQLX_OFFLINE=true` and uses the committed `.sqlx/` snapshot.

### Tables

| Table | Purpose |
|-------|---------|
| `media` | One row per asset. PK: MediaId. Columns: path, filename, size, dates, dimensions, orientation, media_type, is_favorite, is_trashed, trashed_at, duration_ms |
| `thumbnails` | Thumbnail generation state (Pending/Ready/Failed) + file path |
| `media_metadata` | Full EXIF detail (camera, lens, aperture, GPS, etc.) — loaded on demand |
| `albums` | Album metadata (id, name, created_at, updated_at) |
| `album_media` | Album ↔ media membership (album_id, media_id, added_at, position) |
| `people` | Person records (id, name, face_count, is_hidden) |
| `asset_faces` | Face ↔ asset mapping (face_id, asset_id, person_id) |
| `edits` | Non-destructive edit state per media (JSON blob, rendered flag) |
| `sync_checkpoints` | Per-entity-type sync position for Immich incremental sync |
| `sync_audit` | Audit trail of sync operations (entity, action, cycle, status) |
| `upload_queue` | Immich upload queue (path, status, hash) |

## Event System

### EventBus

`src/event_bus.rs` — centralised push-based event delivery using `glib::idle_add_once`. Components subscribe in their own constructors; parents do assembly only.

```
Tokio backend
  → Sender<LibraryEvent> (mpsc, Send)
    → application.rs idle loop
      → translates LibraryEvent → AppEvent
        → EventBus fan-out (glib::idle_add_once)
          → all subscribers (models, views, dialogs)
```

### AppEvent

`src/app_event.rs` — two kinds of variants:

- **Command variants** (`*Requested`) — user intent. E.g. `FavoriteRequested { ids, state }`, `TrashRequested { ids }`.
- **Result variants** (`*Changed`, `Trashed`, etc.) — backend confirmations broadcast to all subscribers.

### CommandDispatcher

`src/commands/dispatcher.rs` — subscribes to the bus, routes `*Requested` events to `CommandHandler` implementations running on the Tokio runtime. Each handler calls the library trait, then emits result events or `AppEvent::Error` on failure.

### Error Toasts

A subscriber in `application.rs` listens for `AppEvent::Error` and shows an `AdwToast` via `WidgetExt::activate_action("win.show-toast", ...)`. Blocking errors (library open/create failures) show `AdwAlertDialog` with error details and recovery options.

## Widget Hierarchy

```
MomentsWindow (adw::ApplicationWindow)
└── main_stack (GtkStack)
    ├── "loading" → spinner
    └── "content" → Gtk.Box
        ├── sidebar: MomentsSidebar (AdwSidebar)
        │   ├── System routes: Photos, Favorites, Recent, People, Albums, Trash
        │   ├── Pinned albums section (dynamic)
        │   └── Status bar (AdwBottomSheet — idle/sync/thumbnails/upload states)
        └── content_stack (ContentCoordinator)
            ├── "empty" → EmptyLibraryView
            ├── "photos" → PhotoGridView (filter=All)
            ├── "favorites" → PhotoGridView (filter=Favorites)
            ├── "recent" → PhotoGridView (filter=RecentImports)
            ├── "trash" → PhotoGridView (filter=Trashed)
            ├── "people" → CollectionGridView (People)
            │   └── drill-down → PhotoGridView (filter=Person)
            └── "albums" → AlbumGridView
                └── drill-down → PhotoGridView (filter=Album)
```

Each view that can drill into photos pushes a `PhotoGridView` onto its internal `NavigationView`. The `PhotoGridView` in turn pushes a `PhotoViewer` or `VideoViewer` when a grid item is activated.

### Key Principles

- **Separate view instances** — each sidebar route gets its own view and model instance. Views are never shared or reused across routes.
- **ContentView trait** — all views implement `ContentView` (widget + optional view_actions). The coordinator installs the active view's action group under the `"view"` prefix.
- **Application singleton** — shared state accessed via `MomentsApplication::default()` with typed accessors (`tokio_handle()`, etc.). Never walk the widget tree.

### GObject Subclassing Pattern

All widgets use the split `imp` module pattern:
- The inner `mod imp` struct holds state and implements GObject trait impls.
- The outer `glib::wrapper!` macro creates the public Rust type.
- Most widgets use Blueprint (`.blp`) declarative templates for static layout, compiled to `.ui` XML at build time.
- Service dependencies are stored as `OnceCell` fields on the imp struct, injected via a `setup()` method after construction.

## Photo Grid

### PhotoGridModel (GObject)

Manages pagination, filtering, and incremental updates. State:
- `store: gio::ListStore` — shared with GridView via MultiSelection
- `filter: MediaFilter` — set at construction, immutable
- `cursor: Option<MediaCursor>` — keyset pagination position
- `id_index: HashMap<MediaId, WeakRef<MediaItemObject>>` — O(1) event routing

`load_more()` dispatches `library.list_media()` to Tokio, processes results back on the GTK thread. Scroll-based lazy loading triggers `load_more()` when within half a page of the bottom.

Subscribes to the event bus for `AssetSynced`, `Trashed`, `Restored`, `Deleted`, `FavoriteChanged` events and updates the store incrementally without full reloads.

### MediaItemObject (GObject)

Wraps `MediaItem` with mutable GObject properties:
- `texture: Option<gdk::Texture>` — starts `None`, set when thumbnail loads.
- `is_favorite: bool` — toggled optimistically by star button.

### Cell Factory

`build_factory()` returns a `SignalListItemFactory` with four callbacks (setup, bind, unbind, teardown). The factory captures `library` and `tokio` so the star button can persist favourite changes without the cell needing backend knowledge.

A `TextureCache` (LRU of decoded RGBA pixels) avoids redundant decode work when cells are recycled. A `Semaphore` limits concurrent thumbnail decodes to half the available CPU cores.

### Zoom

Six levels (96–320px). Stored in GSettings (`zoom-level`). Changing zoom rebuilds the cell factory with the new size — GridView re-layouts automatically.

## Photo Viewer

GObject subclass of `adw::NavigationPage`, pushed onto the `NavigationView` when a grid item is activated. Receives a snapshot of all grid items at activation time for prev/next navigation.

### Full-Resolution Loading

All formats are decoded via the `image` crate with EXIF orientation applied:
1. Resolve original path from library (async DB call)
2. Decode on `tokio::task::spawn_blocking`
3. Apply EXIF orientation
4. Upload RGBA bytes as `gdk::MemoryTexture`

A generation counter (`load_gen`) invalidates stale async results when the user navigates before loading completes.

### Overflow Menu

Clean headerbar: `[★] [ℹ] [✏] [⋮]`. The overflow menu is a manual `gtk::Popover` with icon+label buttons (not `GMenuModel`). Items: Add to album, Share, Export original, Set as wallpaper, Show in Files, Delete.

### Edit Panel

Non-destructive editing with `EditState` (JSON-serialised per-media). The panel has three sections: adjustments (exposure, contrast, saturation, etc.), transforms (rotate/flip), and filters (preset grid + strength slider). Edits are rendered on the Tokio blocking pool via `apply_edits()`.

## Video Viewer

GObject subclass of `adw::NavigationPage` with GStreamer playback. Supports MP4, MOV, MKV, AVI, WebM. Video duration is extracted via `video_meta.rs` using a GStreamer pipeline.

## Albums

- `AlbumGridView` displays all albums with a 2×2 cover mosaic.
- Albums can be created, renamed, pinned to the sidebar, and deleted.
- Photos are added/removed via the `AlbumPickerDialog` (search, thumbnails, inline create).
- Album operations go through the event bus command dispatch pattern.

## People / Faces

Immich backend only. People and face data are synced from the server (`PeopleV1`, `AssetFacesV1` entity types). The `CollectionGridView` displays people with circular thumbnails. People can be renamed, hidden, and drilled into to show their photos.

## Import Pipeline

**Local backend:** `ImportJob::run(sources)` on Tokio walks directories, hashes files (BLAKE3), skips duplicates, copies to `originals/`, inserts DB rows, and spawns `ThumbnailJob` for each asset.

**Immich backend:** `ImmichImportJob` computes SHA-1 (Immich dedup), uploads via the Immich API (`POST /assets`), and inserts into the local cache.

Thumbnails are 360px (longest edge), encoded as WebP, stored in two-level sharded directories (`ab/cd/<hash>.webp`).

## Immich Sync

Background sync polls the Immich server at a configurable interval using `POST /sync/stream` (server-sent events). Entity types: `AssetsV1`, `AssetExifsV1`, `AlbumsV1`, `AlbumToAssetsV1`, `PeopleV1`, `AssetFacesV1`.

The `SyncManager` processes entities with ack-based checkpointing so interrupted syncs resume where they left off. A `ThumbnailDownloader` worker pool downloads asset thumbnails concurrently with throttling.

All synced data is cached in the same SQLite schema as local data. The app works fully offline after initial sync.

## Format Registry

Extensible handler pattern with magic-byte sniffing:

- **StandardHandler** — JPEG, PNG, WebP, TIFF, GIF, HEIC/HEIF (via `image` + `libheif-rs`)
- **RawHandler** — CR2, NEF, ARW, DNG, etc. (via `rawler`)
- **VideoHandler** — MP4, MOV, MKV, AVI, WebM (via GStreamer)

`FormatRegistry` maps extensions to handlers with magic-byte override. Adding a new format is one handler implementation + one `register()` call.

## Persistence (GSettings)

Schema: `io.github.justinf555.Moments`

| Key | Type | Default | Purpose |
|-----|------|---------|---------|
| `library-path` | string | `""` | Path to library bundle (empty = first run) |
| `zoom-level` | uint | `2` | Grid zoom level index (0–5) |
| `album-sort-order` | uint | `0` | Album grid sort order |
| `trash-retention-days` | uint | `30` | Days before auto-purge |
| `sync-interval-seconds` | uint | `300` | Immich sync polling interval |
| `cache-limit-mb` | uint | `500` | Thumbnail cache size limit |
| `pinned-album-ids` | string array | `[]` | Albums pinned to sidebar |

Settings that affect background tasks use `tokio::sync::watch` channels for live updates from the Preferences dialog without restarting the app.

## Build System

- **Meson** — builds Rust code, compiles Blueprints, generates GResources
- **Flatpak** — packages the app with GNOME 50 Platform runtime
- **Blueprint** — UI template language (`.blp` → `.ui` XML at build time)
- **GResources** — compiled resource bundle loaded at startup

The Flatpak manifest pulls from the local git repo, so changes must be committed before `make run`. The dev manifest (`make run-dev`) uses `type: "dir"` to pick up working tree changes without committing.

```bash
make run          # Build + install Flatpak (release, requires commit)
make run-dev      # Build + install Flatpak (debug, uncommitted changes OK)
make lint         # cargo clippy (inside Flatpak SDK)
make test         # cargo test (unit tests)
make test-integration  # headless GTK integration tests (requires Wayland)
make ci-all       # lint + test + test-integration + audit
```

## Testing

- **Unit tests** — `#[cfg(test)]` modules alongside the code they test. Async tests use `#[tokio::test]`. Run via `make test`.
- **Integration tests** — headless GTK4 tests under `tests/`, run with `make test-integration` inside a mutter headless Wayland session. Cover event bus wiring, PhotoGridModel behaviour, and widget construction.
- **Coverage** — `make coverage` generates an HTML report via `cargo-llvm-cov`.
- **CI** — GitHub Actions runs lint, unit tests, integration tests, coverage, cargo audit, and cargo deny on every PR.

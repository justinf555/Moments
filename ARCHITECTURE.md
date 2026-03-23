# Architecture

This document describes the architecture of Moments as of March 2026. It is intended for engineers joining the project — read this before diving into the code.

## Overview

Moments is a GNOME photo management app written in Rust, targeting GNOME Circle. It uses GTK4 + libadwaita for the UI, SQLite for persistence, and a trait-based library abstraction that supports multiple backends (local filesystem now, Immich planned).

The app has two async executors that must never be confused:

- **GTK executor** (`glib::MainContext`) — UI thread only. Widget updates, signal handlers, property bindings.
- **Tokio executor** — all backend I/O: database queries, file operations, image decoding, future HTTP calls.

Results flow from Tokio → GTK via a `std::sync::mpsc` channel polled in a `glib::idle_add_local` callback.

## Module Map

```
src/
  main.rs              Entry point: tracing, gettext, GResources, Tokio runtime, app.run()
  application.rs       MomentsApplication — lifecycle, GActions, event polling
  config.rs            Compile-time constants (VERSION, PKGDATADIR, etc.)

  library.rs           Library supertrait (blanket impl of all feature sub-traits)
  library/
    storage.rs         LibraryStorage — open/close lifecycle
    import.rs          LibraryImport — batch folder import
    media.rs           LibraryMedia — CRUD + queries; MediaId, MediaItem, MediaFilter, MediaCursor
    thumbnail.rs       LibraryThumbnail — thumbnail path resolution + DB state
    viewer.rs          LibraryViewer — resolve original file path
    error.rs           LibraryError (thiserror)
    event.rs           LibraryEvent enum (Ready, ThumbnailReady, ImportComplete, etc.)
    config.rs          LibraryConfig (Local | Immich)
    bundle.rs          On-disk library structure (originals/, thumbnails/, database/)
    factory.rs         LibraryFactory — constructs concrete backends, returns Arc<dyn Library>
    db.rs              Database (sqlx::SqlitePool) — implements LibraryMedia + thumbnail methods
    db/migrations/     Numbered SQL migrations (embedded via sqlx::migrate!)
    exif.rs            EXIF metadata extraction (kamadak-exif)
    format/            FormatHandler trait + registry (StandardHandler, RawHandler)
    importer.rs        ImportJob — walks directories, hashes, copies, inserts, spawns thumbnails
    thumbnailer.rs     ThumbnailJob — decode, resize, encode WebP, write atomically
    providers/
      local.rs         LocalLibrary — implements all Library sub-traits, delegates to Database

  ui.rs                ContentView trait; module re-exports
  ui/
    window.rs          MomentsWindow — main shell, wires sidebar → coordinator → views
    coordinator.rs     ContentCoordinator — routes sidebar selection to GtkStack children
    sidebar.rs         MomentsSidebar (ListBox with route rows)
    sidebar/
      route.rs         SidebarRoute config (static array of id/label/icon)
      row.rs           MomentsSidebarRow widget
    photo_grid.rs      PhotoGrid (widget) + PhotoGridView (ContentView impl with NavigationView)
    photo_grid/
      model.rs         PhotoGridModel — pagination, lazy thumbnail loading, event handling
      item.rs          MediaItemObject (GObject wrapper around MediaItem)
      cell.rs          PhotoGridCell — thumbnail + spinner + star button overlay
      factory.rs       SignalListItemFactory — creates/binds/unbinds cells
    viewer.rs          PhotoViewer — full-res display, prev/next navigation, star toggle
    viewer/
      info_panel.rs    EXIF metadata display panel
    empty_library.rs   EmptyLibraryView — AdwStatusPage shown when library has no photos
    import_dialog.rs   Import progress dialog
    setup_window.rs    First-run setup wizard
```

## Library Abstraction

The GTK layer only sees `Arc<dyn Library>`. It never imports or references concrete backend types.

`Library` is a blanket-impl composition of feature sub-traits:

```
Library = LibraryStorage + LibraryImport + LibraryMedia + LibraryThumbnail + LibraryViewer
```

Each sub-trait is defined in its own file and adds one capability. New features add new sub-traits without modifying existing ones.

`LibraryFactory` is the single place where concrete types (`LocalLibrary`) are named and constructed. Everything else works through trait objects.

### MediaId

Every asset is identified by a `MediaId` — the 64-character lowercase hex BLAKE3 hash of the file's raw bytes. This is the primary key in the database, the thumbnail filename, and the deduplication key. Hashing uses `tokio::task::spawn_blocking` with a streaming hasher so large files are never fully loaded into memory.

### MediaFilter

Queries accept a `MediaFilter` enum (`All` | `Favorites`). The filter maps to a SQL WHERE clause fragment via a strategy pattern in `Database::list_media`. New filters (e.g., `RecentImports`) add a variant and a clause — no other changes needed.

### Keyset Pagination

`list_media` uses keyset pagination via `MediaCursor` (last seen `COALESCE(taken_at, 0)` + `id`). This is O(1) per page regardless of library size — no OFFSET scans.

## Database

SQLite via `sqlx` with an async connection pool. The `Database` struct wraps a `SqlitePool` and provides typed CRUD methods.

Schema is managed by numbered migrations in `src/library/db/migrations/`, embedded at compile time via `sqlx::migrate!`. Every schema change must be a new migration file — no ad-hoc DDL in code.

### Current Tables

| Table | Purpose |
|-------|---------|
| `media` | One row per asset. PK: BLAKE3 hash. Columns: path, filename, size, dates, dimensions, orientation, media_type, is_favorite |
| `thumbnails` | Thumbnail generation state (Pending/Ready/Failed) + file path |
| `media_metadata` | Full EXIF detail (camera, lens, aperture, GPS, etc.) — loaded on demand |

## Event System

Backend → UI communication uses a `std::sync::mpsc` channel:

```
Tokio (ImportJob, ThumbnailJob)
  → Sender<LibraryEvent>
    → application.rs idle loop (glib::idle_add_local)
      → routes events to models and dialogs
```

Events:
- `ThumbnailReady { media_id }` — broadcast to all `PhotoGridModel` instances
- `ImportProgress { current, total }` — forwarded to `ImportDialog`
- `ImportComplete(summary)` — closes dialog, reloads all models

The idle callback polls `try_recv()` in a loop until `Empty`, then yields. This runs every GTK tick (~16ms).

## Widget Hierarchy

```
MomentsWindow (adw::ApplicationWindow)
└── main_stack (GtkStack)
    ├── "loading" → spinner
    └── "content" → NavigationSplitView
        ├── sidebar: MomentsSidebar (ListBox)
        │   ├── "photos"    → Photos
        │   └── "favorites" → Favorites
        └── content: NavigationPage
            └── content_stack (GtkStack, ContentCoordinator)
                ├── "empty" → EmptyLibraryView (AdwStatusPage)
                ├── "photos" → PhotoGridView (filter=All)
                │   └── NavigationView
                │       ├── "grid" → ToolbarView
                │       │   ├── HeaderBar [import, zoom, menu]
                │       │   └── PhotoGrid → ScrolledWindow → GridView
                │       │       └── cells: Picture + Spinner + Star button
                │       └── "viewer" → ToolbarView (pushed on activation)
                │           ├── HeaderBar [star, info toggle]
                │           └── OverlaySplitView
                │               ├── Picture + Spinner + prev/next
                │               └── InfoPanel
                └── "favorites" → PhotoGridView (filter=Favorites)
                    └── (same structure as "photos")
```

### Key Principle: Separate View Instances

Each sidebar route gets its own `PhotoGridView` + `PhotoGridModel` instance. Views are never shared or reused across routes. Each independently maintains scroll position, zoom level, and viewer state. Switching routes is instant — no reload.

### ContentCoordinator

Simple router: maps sidebar route IDs to `ContentView` implementations stored as `GtkStack` children. `navigate(id)` calls `stack.set_visible_child_name(id)`.

### Empty/Content Toggle

The photos model's `store.connect_items_changed` switches the stack between "empty" and "photos" based on item count. This uses `stack.set_visible_child_name` directly — not `coordinator.navigate()` — to avoid re-entrant borrows during `on_page_loaded`.

## Photo Grid

### PhotoGridModel

Plain Rust struct (not GObject), wrapped in `Rc`, lives on the GTK thread only.

State:
- `store: gio::ListStore` — shared with GridView via MultiSelection
- `filter: Cell<MediaFilter>` — set at construction, immutable
- `cursor: RefCell<Option<MediaCursor>>` — keyset pagination position
- `id_index: HashMap<MediaId, WeakRef<MediaItemObject>>` — O(1) thumbnail event routing

`load_more()` dispatches `library.list_media()` to Tokio, processes results back on the GTK thread. Scroll-based lazy loading triggers `load_more()` when within half a page of the bottom.

### MediaItemObject (GObject)

Wraps `MediaItem` with two mutable GObject properties:
- `texture: Option<gdk::Texture>` — starts `None`, set when thumbnail loads. Cells bind to `notify::texture`.
- `is_favorite: bool` — toggled optimistically by star button. Cells bind to `notify::is-favorite`.

### Cell Factory

`build_factory(cell_size, library, tokio)` returns a `SignalListItemFactory` with four callbacks:

1. **setup** — create `PhotoGridCell`, set size from zoom level
2. **bind** — connect cell to item signals, wire star button click → optimistic toggle + async `set_favorite`
3. **unbind** — disconnect all signals, reset cell visual state
4. **teardown** — remove child widget

The factory captures `library` and `tokio` so the star button can persist favourite changes without the cell needing backend knowledge.

### Zoom

Six levels (96–320px). Stored in GSettings (`zoom-level`). Changing zoom rebuilds the cell factory with the new size — GridView re-layouts automatically.

## Photo Viewer

Pushed onto the `NavigationView` when a grid item is activated. Receives a snapshot of all grid items at activation time for prev/next navigation.

### Full-Resolution Loading

All formats are decoded via the `image` crate with EXIF orientation applied:
1. Resolve original path from library (async DB call)
2. Decode on `tokio::task::spawn_blocking`
3. Apply EXIF orientation via `apply_orientation()`
4. Upload RGBA bytes as `gdk::MemoryTexture`

A generation counter (`load_gen`) invalidates stale async results when the user navigates to a different photo before loading completes.

## Import Pipeline

`ImportJob::run(sources)` on Tokio:

1. Walk directories recursively, collect candidate files
2. For each file:
   - Check extension against `FormatRegistry`
   - Hash (BLAKE3) + extract EXIF in one `spawn_blocking` pass
   - Skip if duplicate (DB lookup by MediaId)
   - Copy to `originals/` (date-based or source-relative path)
   - Insert `media` row + `media_metadata` row
   - Spawn `ThumbnailJob` (decode → resize → encode WebP → write)
   - Emit progress events
3. Emit `ImportComplete` with summary

Thumbnails are 360px (longest edge), encoded as WebP, stored in two-level sharded directories (`ab/cd/<hash>.webp`).

## Format Registry

Extensible handler pattern:

```rust
pub trait FormatHandler: Send + Sync {
    fn extensions(&self) -> &[&str];
    fn decode(&self, path: &Path) -> Result<DynamicImage, LibraryError>;
}
```

- **StandardHandler** — JPEG, PNG, WebP, TIFF, GIF, HEIC/HEIF (via `image` + `libheif-rs`)
- **RawHandler** — CR2, NEF, ARW, DNG, etc. (via `rawler`)

`FormatRegistry` maps extensions to handlers. Adding a new format is one handler implementation + one `register()` call.

## Favourites

Boolean `is_favorite` column on `media`. Toggled via `LibraryMedia::set_favorite()`.

UI uses **optimistic updates**: the star button immediately sets `is_favorite` on the `MediaItemObject` (GObject property change → cell repaints), then persists asynchronously. If the DB write fails, the error is logged but the UI is not rolled back.

Each sidebar route has its own model, so starring a photo in "Photos" does not immediately appear in "Favorites" (tracked as issue #63).

## Persistence (GSettings)

Schema: `io.github.justinf555.Moments`

| Key | Type | Default | Purpose |
|-----|------|---------|---------|
| `library-path` | string | `""` | Path to library bundle (empty = first run) |
| `zoom-level` | uint | `2` | Grid zoom level index (0–5) |

## Build System

- **Meson** — builds Rust code, compiles Blueprints, generates GResources
- **Flatpak** — packages the app with GNOME Platform runtime
- **Blueprint** — UI template language (`.blp` → `.ui` XML at build time)
- **GResources** — compiled resource bundle loaded at startup (templates, icons)

Changes must be committed to git before rebuilding (`make run`) because the Flatpak manifest pulls from the local git repo.

## Error Handling

All library operations return `Result<T, LibraryError>`. The `LibraryError` enum uses `thiserror` for ergonomic `?` propagation.

Current approach: errors are logged via `tracing` but not surfaced to the user. User-visible error handling (toasts, dialogs) is tracked as issue #67.

## Testing

Unit tests in `#[cfg(test)]` modules alongside the code they test. Async tests use `#[tokio::test]`. Run via `cargo test` (works outside Flatpak).

Integration tests require Flatpak and are exercised manually via `make run`.

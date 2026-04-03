# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

This project uses **Meson** as its build system and is packaged as a **Flatpak**. It must be built and run through Flatpak — do not attempt to run the binary directly.

```bash
# Build and run via Flatpak (primary workflow)
make run

# Clean the Flatpak build directory
make clean
```

The Flatpak manifest is `io.github.justinf555.Moments.json` (local dev) and `io.github.justinf555.Moments.flathub.json` (Flathub submission). The local manifest pulls source from the local git repo (`file:///home/justin/Projects/Moments`, branch `main`), so **changes must be committed before rebuilding**. The `make run` command installs the Flatpak locally (`--user --install`) so icons are exported to GNOME Shell.

### Dev build

```bash
# Build with editing feature enabled, debug logs, installs as Flatpak
make run-dev

# Release: creates PR with version bump, merging triggers tag + GitHub Release
make release VERSION=0.2.0
```

The dev manifest (`io.github.justinf555.Moments.dev.json`) uses `type: "dir"` — picks up working tree changes without committing. It installs under the **same app ID** as production (`io.github.justinf555.Moments`) so Flatpak portals work correctly. `flatpak-builder --run` does not support portals.

GNOME Builder can also use the dev manifest — configure it in the project build settings.

Instruct the user to test via GNOME Builder or `make run-dev` — do not attempt to run the app binary directly.

## Architecture

Moments is a GNOME/GTK4 photo management app written in Rust, targeting GNOME Circle. It uses:

- **GTK4** (`gtk4` crate) + **libadwaita** (`libadwaita` crate) for the UI
- **GLib/GObject** subclassing pattern throughout — every widget and application type uses the `mod imp {}` pattern with `#[glib::object_subclass]`
- **`gettextrs`** for i18n
- **`async-trait`** for async trait definitions
- **Tokio** (`tokio` crate) as the library executor — created in `main()`, shared across all backends via `tokio::runtime::Handle`
- **`thiserror`** for error types

### Module structure

```
src/
  main.rs              — Entry point: sets up gettext, loads GResources, creates MomentsApplication
  application.rs       — MomentsApplication (adw::Application subclass); registers GActions
  config.rs            — Compile-time constants (VERSION, PKGDATADIR, etc.)
  library.rs           — Library supertrait (composition of feature sub-traits)
  library/
    storage.rs         — LibraryStorage async trait (open/close, set_sync_interval, set_cache_limit)
    media.rs           — MediaId, MediaItem, MediaFilter, LibraryMedia trait (incl. library_stats)
    album.rs           — AlbumId, Album, LibraryAlbums trait
    faces.rs           — PersonId, Person, LibraryFaces trait
    editing.rs         — EditState types, LibraryEditing trait (non-destructive editing)
    edit_renderer.rs   — apply_edits() pure function (exposure, color, transforms, filters)
    import.rs          — LibraryImport trait
    thumbnail.rs       — LibraryThumbnail trait, sharded path helpers
    viewer.rs          — LibraryViewer trait (original file access)
    error.rs           — LibraryError enum (thiserror-based)
    event.rs           — LibraryEvent enum (channel-based backend → GTK communication)
    db.rs              — Database struct (sqlx::SqlitePool), LibraryStats, ServerStats
    db/media.rs        — LibraryMedia impl, MediaRow, filter_clause/sort_expr
    db/albums.rs       — LibraryAlbums impl
    db/edits.rs        — Edit state CRUD (get/upsert/delete/mark_rendered)
    db/faces.rs        — People/face CRUD (upsert, list, face_count maintenance)
    db/sync.rs         — Sync upserts, checkpoints, audit methods
    db/thumbnails.rs   — Thumbnail status tracking
    db/stats.rs        — Aggregate library statistics query
    db/upload.rs       — Upload queue CRUD
    db/migrations/     — Numbered SQL migrations (001–014)
    bundle.rs          — Library bundle on disk (manifest, paths)
    config.rs          — LibraryConfig enum (Local / Immich)
    factory.rs         — LibraryFactory (creates backends by config type)
    immich_client.rs   — ImmichClient (HTTP client for Immich API)
    immich_importer.rs — ImmichImportJob (upload to Immich server)
    importer.rs        — Local import job (walk_dir, collect_candidates)
    keyring.rs         — GNOME Keyring integration (session token storage)
    sync.rs            — SyncManager + ThumbnailDownloader + CacheEvictor (Immich background tasks)
    format/            — Format detection (magic bytes, standard/raw/video handlers)
    providers/
      local.rs         — LocalLibrary (local filesystem backend)
      immich.rs        — ImmichLibrary (Immich server backend)
  app_event.rs         — AppEvent enum (commands, results, lifecycle events)
  event_bus.rs         — EventBus (push-based fan-out delivery via glib::idle_add_once)
  commands/
    dispatcher.rs      — CommandDispatcher (routes *Requested events to handlers on Tokio)
    trash.rs           — TrashCommand handler
    restore.rs         — RestoreCommand handler
    delete.rs          — DeleteCommand handler
    favorite.rs        — FavoriteCommand handler
    add_to_album.rs    — AddToAlbumCommand handler
    remove_from_album.rs — RemoveFromAlbumCommand handler
    create_album.rs    — CreateAlbumCommand handler
  ui/
    window.rs          — MomentsWindow; wires sidebar, coordinator, views
    sidebar.rs         — MomentsSidebar with dynamic album section + persistent status bar
    sidebar/
      route.rs         — TOP_ROUTES / BOTTOM_ROUTES definitions (Photos, Favorites, Recent, People, Trash)
      row.rs           — MomentsSidebarRow widget
    coordinator.rs     — ContentCoordinator (stack-based view routing, returns view_actions)
    collection_grid.rs — CollectionGridView (reusable grid for People, future Memories/Places)
    collection_grid/
      cell.rs          — CollectionGridCell widget (thumbnail + name + subtitle)
      factory.rs       — Cell factory with ThumbnailStyle (Circular/Square)
      item.rs          — CollectionItemObject (GObject wrapper for collection items)
    photo_grid.rs      — PhotoGridView (zoom, selection, empty states, viewer integration)
    photo_grid/
      actions.rs       — Action wiring: context menu, album controls
      action_bar.rs    — Selection mode action bar (favourite, album, trash/restore/delete)
      model.rs         — PhotoGridModel (pagination, filtering, incremental updates, bus errors)
      factory.rs       — Cell factory (bind/unbind with texture management + decode semaphore)
      cell.rs          — PhotoGridCell widget (placeholder → thumbnail → star + checkbox)
      item.rs          — MediaItemObject (GObject wrapper for grid items)
      texture_cache.rs — LRU cache for decoded RGBA thumbnail pixels
    viewer.rs          — PhotoViewer (full-res display, overflow menu, edit session)
    viewer/
      info_panel.rs    — EXIF metadata display panel
      edit_panel.rs    — Edit panel with exposure/color sliders, transform controls
    video_viewer.rs    — VideoViewer (GStreamer playback, matching overflow menu)
    album_dialogs.rs   — Create/rename/delete album dialogs
    album_picker_dialog.rs — Album picker dialog entry point (async data fetch + present)
    album_picker_dialog/
      dialog.rs        — AdwDialog with search, thumbnails, create flow, empty state
      album_row.rs     — Album row widget (thumbnail + name + count + checkmark + pill)
      state.rs         — AlbumPickerData, AlbumEntry (data-in, events-out)
    import_dialog.rs   — Import progress dialog
    preferences_dialog.rs — Preferences (sentence case, AdwSpinRow, library stats)
    empty_library.rs   — Empty library placeholder view
    setup_window/      — Setup wizard (backend picker, local setup, Immich setup)
    widgets.rs         — Shared UI components (expander_row, detail_row, section_label)
  style.css            — Custom CSS (selection highlight, circular thumbnails, hidden person styling)
```

### GTK/GObject subclassing pattern

All GObject types follow the split `imp` module pattern:
- The inner `mod imp` struct holds state and implements GObject trait impls
- The outer `glib::wrapper!` macro creates the public Rust type
- UI templates are declared with `#[template(resource = "...")]` and bound in `class_init`/`instance_init`

### Two-executor model

The app has two distinct async executors that must never be confused:

- **GTK executor** (`glib::MainContext`) — UI thread only. Runs widget updates, signal handlers, and calls into library traits via `glib::MainContext::default().spawn_local()`.
- **Library executor** (Tokio runtime) — all backend I/O: database queries, file ops, future Immich HTTP calls. Created in `main()` before `app.run()` and held for the process lifetime. All backends share it via `tokio::runtime::Handle` stored on `MomentsApplication`.

Results flow back from Tokio → GTK via `Sender<LibraryEvent>` (a `std::sync::mpsc` channel, which is `Send`). The idle loop in `application.rs` translates `LibraryEvent` → `AppEvent` and sends via the event bus for fan-out delivery to all subscribers.

### Library abstraction layer

`Library` (in `library.rs`) is a blanket-impl composition of feature sub-traits: `LibraryStorage + LibraryImport + LibraryMedia + LibraryThumbnail + LibraryViewer + LibraryAlbums + LibraryFaces + LibraryEditing`. All backend work runs on the Tokio executor. `LibraryStorage::open()` receives a `tokio::runtime::Handle` which is stored for the backend's lifetime.

Two backends exist:
- **`LocalLibrary`** (`providers/local.rs`) — stores originals on disk, generates thumbnails locally
- **`ImmichLibrary`** (`providers/immich.rs`) — syncs with an Immich server via `POST /sync/stream`, caches everything locally in the same SQLite schema. Background sync polls at a configurable interval (GSettings `sync-interval-seconds`, live-updatable) with a thumbnail download worker pool. Also syncs people and face data (`PeopleV1`, `AssetFacesV1`). See `docs/design-immich-backend.md` and `docs/design-face-integration.md` for the full design.

### Database

`src/library/db.rs` — backend-agnostic `Database` struct wrapping an `sqlx::SqlitePool`. Used by all backends that need persistence. Migrations live at `src/library/db/migrations/` (001–013) and are embedded via `sqlx::migrate!()`. **Every schema change must be a numbered migration — no ad-hoc `CREATE TABLE IF NOT EXISTS` in code.** Query implementations are split into submodules: `db/media.rs`, `db/albums.rs`, `db/faces.rs`, `db/sync.rs`, `db/thumbnails.rs`, `db/stats.rs`, `db/upload.rs`.

After any schema change, regenerate the offline query snapshot:
```bash
# Requires DATABASE_URL pointing at a database with the current schema
cargo sqlx database create
cargo sqlx migrate run
cargo sqlx prepare    # regenerates .sqlx/ — commit this directory
```

CI sets `SQLX_OFFLINE=true` and uses the committed `.sqlx/` snapshot.

### MediaId

`src/library/media.rs` — `MediaId` is the primary identity for every asset. For the local backend, it is a 64-char lowercase hex BLAKE3 hash (content-addressable). For the Immich backend, it is the server's UUID. The `MediaId` newtype treats both as opaque strings — the grid, viewer, and thumbnail pipeline don't care which format is used.

### Application singleton pattern

Access shared application state (Tokio handle, settings, etc.) via `MomentsApplication::default()` with typed accessors like `tokio_handle()`. Don't walk the widget tree with `.root().application()`. This follows the standard GNOME Rust pattern (Fractal, Planify).

### Sidebar status bar

The sidebar bottom bar is a persistent `AdwBottomSheet` with a `GtkStack` that switches between five states: Idle ("Synced X ago"), Sync ("Syncing..."), Thumbnails ("Thumbnails X/Y"), Upload (expandable with progress bar), and Complete ("Upload Complete"). Priority-based: upload > sync > thumbnails > idle. See `docs/design-sidebar-status-bar.md`.

Import button and hamburger menu live in the **sidebar header** — content headers have view-specific controls only (zoom, selection).

### Live-update pattern (watch channels)

Settings that affect background tasks (sync interval, cache limit) use `tokio::sync::watch` channels for live updates from the Preferences dialog without restarting the app:

1. GSettings value read on GTK thread at startup → initial `watch::channel` value
2. Background task reads via `borrow_and_update()` each cycle (must use `_and_update` to avoid re-triggering `changed()`)
3. Preferences dialog calls `lib.set_sync_interval()` / `lib.set_cache_limit()` which sends to the watch
4. `tokio::select!` on sleep + `changed()` wakes the task immediately on value change
5. `LibraryStorage` trait has default no-op methods — only Immich backend implements them

### ContentView trait and view actions

All content views implement `ContentView` (widget + optional view_actions). When the coordinator navigates to a view, its action group is installed on the window under the `"view"` prefix. This is critical for zoom buttons to work across different views (Photos, Favorites, Albums, People drill-down). When a `CollectionGridView` pushes a `PhotoGridView` onto its internal NavigationView, it must also install that view's actions on the window.

### Event bus and command dispatch

`src/event_bus.rs` — centralised push-based event delivery using `glib::idle_add_once`. Components subscribe in their own constructors; parents do assembly only.

- **`AppEvent`** (`app_event.rs`) — command variants (`*Requested`) and result variants (`*Changed`, `Trashed`, etc.)
- **`EventSender`** — `Send + Clone` wrapper around `mpsc::Sender`. Safe to call from Tokio threads.
- **`CommandDispatcher`** (`commands/dispatcher.rs`) — subscribes to the bus, routes `*Requested` events to `CommandHandler` impls on the Tokio runtime. Each handler emits result events or `AppEvent::Error` on failure.
- **Error toasts** — a subscriber in `application.rs` listens for `AppEvent::Error` and shows an `AdwToast` via `WidgetExt::activate_action("win.show-toast", ...)`. **Important:** Use `WidgetExt::activate_action` (not `ActionGroupExt::activate_action`) — `ActionGroupExt` does not resolve the `win.` prefix.
- **Blocking errors** — library open/create failures show `AdwAlertDialog` with error details and recovery options.

### Viewer headerbar and overflow menu

Photo and video viewers use a clean headerbar: `[★] [ℹ] [✏] [⋮]`. The overflow menu (⋮) is a manual `gtk::Popover` with icon+label buttons (not `GMenuModel` — GTK4's `PopoverMenu` does not render icons from `GMenuModel` attributes). Items: Add to album, Share, Export original, Set as wallpaper (photo only), Show in Files, Delete (destructive, separated). Shared builder: `build_viewer_menu_popover()` and `find_menu_button()` in `viewer.rs`.

### Album picker dialog

`src/ui/album_picker_dialog.rs` — full `adw::Dialog` replacing the old popover. Architecture: async data fetch → `AlbumPickerData` (plain structs) → dialog construction → `AppEvent` bus commands. The dialog never imports `Library`. Features: search with Pango bold highlighting, cover thumbnails (pre-decoded on Tokio), "Already added" pills, inline "New album…" creation flow, empty state.

### Icons

Use only icons confirmed to exist in the Adwaita icon theme. Common ones: `object-select-symbolic` (checkmark), `view-refresh-symbolic` (sync), `view-conceal-symbolic` (eye-slash/hidden), `folder-download-symbolic`, `go-up-symbolic`, `document-send-symbolic`. Check with `find /usr/share/icons/Adwaita -name "icon-name.svg"` before using.

## Tracing / logging

All log output uses the `tracing` crate — never `println!` or `eprintln!`.

- `tracing_subscriber` is initialised in `main()` with `EnvFilter::from_default_env()`; default level is `info`, control verbosity with `RUST_LOG=moments=debug`
- Use `#[instrument]` on every function worth timing (async backend methods, factory calls, bundle open/create)
- Use `#[instrument(skip(field))]` to omit large or sensitive parameters from spans
- Level guidance: `error!` — unrecoverable; `warn!` — degraded but continuing; `info!` — lifecycle milestones (start, open, close); `debug!` — per-operation detail

## Code conventions

- Use Rust 2018+ module naming: place submodules in `src/foo/bar.rs`, never `src/foo/bar/mod.rs`
- Prefer many small, focused files over large ones
- Every feature must be developed on a dedicated git branch — never commit directly to `main` without branching first
- GTK dependency versions are pinned together — keep `gtk4` and `libadwaita` version-aligned when upgrading

## Design documents

Design docs live in `docs/` and follow a consistent format with issue links, status, ASCII diagrams, and implementation phases:

- `docs/design-immich-backend.md` — Immich backend architecture, offline-first sync, trait status table
- `docs/design-face-integration.md` — People/face sync, DB schema, UI, management
- `docs/design-sidebar-status-bar.md` — Persistent status bar states, button relocation, event flow
- `docs/design-lazy-view-loading.md` — Lazy view registration pattern
- `docs/design-video-import.md` — Video format detection and import
- `docs/design-photo-editing.md` — Non-destructive editing: data model, renderer, UI, Immich integration
- `docs/design-event-bus.md` — EventBus architecture, AppEvent enum, CommandDispatcher pattern
- `docs/design-integration-testing.md` — Headless GTK4 testing with mutter, CI config, coverage tracking

### Feature flags

The `editing` Cargo feature gates the edit button in the viewer. It is disabled by default but will be enabled for the next Flathub release (v0.2.0). Enable with `cargo run --features editing` for development.

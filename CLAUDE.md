# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Pre-commit Checks

Always run `make lint` before committing or creating PRs. If a Makefile exists, check available targets with `make help` or inspect the Makefile first.

## Build & Run

This project uses **Meson** as its build system and is packaged as a **Flatpak**. It must be built and run through Flatpak — do not attempt to run the binary directly.

```bash
# Build and run via Flatpak (primary workflow)
make run

# Build with editing feature, debug logs, installs as Flatpak
make run-dev

# Clean the Flatpak build directories
make clean
```

The Flatpak manifest is `io.github.justinf555.Moments.json` (local dev) and `io.github.justinf555.Moments.flathub.json` (Flathub submission). The local manifest pulls source from the local git repo (`file:///home/justin/Projects/Moments`, branch `main`), so **changes must be committed before rebuilding**. The `make run` command installs the Flatpak locally (`--user --install`) so icons are exported to GNOME Shell.

### Dev build

The dev manifest (`io.github.justinf555.Moments.dev.json`) uses `type: "dir"` — picks up working tree changes without committing. It uses a **separate state dir** (`.flatpak-builder-dev/`) so switching between `make run` and `make run-dev` doesn't invalidate the cargo cache. It installs under a **separate app ID** (`io.github.justinf555.Moments.Devel`) so dev and production can run side-by-side with separate GSettings, data dirs, and keyring entries.

### Build profiles

The Meson option `-Dprofile=development` (set automatically by the dev manifest) switches the app ID to `io.github.justinf555.Moments.Devel` and enables the GNOME "devel" visual style (striped headerbar). The `config::APP_ID` and `config::PROFILE` constants in `config.rs.in` are set at build time — **never hardcode the app ID string in Rust code**; always use `config::APP_ID`.

```bash
# Release: creates PR with version bump, merging triggers tag + GitHub Release
make release VERSION=0.2.0

# Testing & linting (all run inside Flatpak SDK)
make lint              # cargo clippy
make test              # cargo test (unit tests)
make test-integration  # headless GTK integration tests (requires Wayland)
make check             # cargo check
make coverage          # cargo-llvm-cov HTML report
make metrics           # complexity report (top 20 functions)
make ci-all            # lint + test + test-integration + audit
make audit             # cargo audit + cargo deny
```

GNOME Builder can also use the dev manifest — configure it in the project build settings.

Instruct the user to test via GNOME Builder or `make run-dev` — do not attempt to run the app binary directly.

## Architecture & Platform Constraints

This is a Rust/GTK4 GNOME application using Flatpak. Key constraints:
- GObject subclassing requires careful handling of Cell/RefCell, Debug traits, and WeakRef lifetimes
- Always register new source files in meson.build
- Use `WidgetExt` not `ActionGroupExt` for toast/action patterns
- GTK cell virtualization means widget positions aren't stable — never assume fixed grid positions
- Avoid nested Flatpak sandbox operations (flatpak-builder --run inside Flatpak won't work)

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
    db/media.rs        — LibraryMedia impl, MediaRow, filter_clause/sort_expr (read queries)
    db/media_write.rs  — Insert, favourite, trash, restore, delete (write queries)
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
    sync.rs            — SyncHandle (public API: start, shutdown, set_interval)
    sync/
      manager.rs       — SyncManager (sync loop, entity handlers, ack flushing)
      downloader.rs    — ThumbnailDownloader worker pool
      types.rs         — Immich sync protocol DTOs and parse helpers
      tests.rs         — Unit tests for sync manager and handlers
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
    sidebar.rs         — MomentsSidebar (AdwSidebar) with pinned albums + persistent status bar
    sidebar/
      route.rs         — ROUTES definitions (Photos, Favorites, Recent, People, Albums, Trash)
    coordinator.rs     — ContentCoordinator (stack-based view routing)
    collection_grid.rs — CollectionGridView (GObject + Blueprint, reusable grid for People)
    collection_grid/
      actions.rs       — Drill-down, context menu (rename, hide/unhide)
      cell.rs          — CollectionGridCell widget (thumbnail + name + subtitle)
      factory.rs       — Cell factory with ThumbnailStyle (Circular/Square)
      item.rs          — CollectionItemObject (GObject wrapper for collection items)
    photo_grid.rs      — PhotoGrid (inner GObject) + PhotoGridView (GObject + Blueprint, zoom, selection, viewer integration)
    photo_grid/
      actions.rs       — Context menu (per-action handlers), album controls
      action_bar.rs    — Selection mode action bar (favourite, album, trash/restore/delete)
      model.rs         — PhotoGridModel (GObject, pagination, filtering, incremental updates, bus events)
      factory.rs       — Cell factory (bind/unbind with texture management + decode semaphore)
      cell.rs          — PhotoGridCell widget (placeholder → thumbnail → star + checkbox)
      item.rs          — MediaItemObject (GObject wrapper for grid items)
      texture_cache.rs — LRU cache for decoded RGBA thumbnail pixels
    viewer.rs          — PhotoViewer (GObject + Blueprint, adw::NavigationPage subclass)
    viewer/
      loading.rs       — Full-res decode, edit session setup, metadata fetching
      menu.rs          — Shared overflow menu builder + photo viewer menu wiring
      info_panel.rs    — EXIF metadata display panel
      edit_panel.rs    — EditPanel (GObject + Blueprint, session mgmt, save/revert, render)
      edit_panel/
        transforms.rs  — Rotate/flip buttons
        filters.rs     — Filter preset grid + strength slider
        sliders.rs     — Adjust sliders (exposure, colour)
    video_viewer.rs    — VideoViewer (GObject + Blueprint, adw::NavigationPage subclass)
    album_grid.rs      — AlbumGridView (GObject + Blueprint, sort, empty state, bus subscription)
    album_grid/
      actions.rs       — Context menu (open, rename, pin, delete) + drill-down helper
      selection.rs     — Enter/exit selection mode, batch delete
      card.rs          — AlbumCard widget (cover mosaic, name, count, checkbox)
      factory.rs       — Card factory (bind/unbind with cover thumbnail loading)
      item.rs          — AlbumItemObject (GObject wrapper for album items)
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

`src/library/db.rs` — backend-agnostic `Database` struct wrapping an `sqlx::SqlitePool`. Used by all backends that need persistence. Migrations live at `src/library/db/migrations/` (001–014) and are embedded via `sqlx::migrate!()`. **Every schema change must be a numbered migration — no ad-hoc `CREATE TABLE IF NOT EXISTS` in code.** Query implementations are split into submodules: `db/media.rs`, `db/albums.rs`, `db/faces.rs`, `db/sync.rs`, `db/thumbnails.rs`, `db/stats.rs`, `db/upload.rs`.

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

### Sidebar

The sidebar uses `AdwSidebar` with routes defined in `sidebar/route.rs`. People route is hidden for the Local backend (no face detection). Pinned albums are added dynamically as a separate `AdwSidebarSection`.

The sidebar bottom bar is a persistent `AdwBottomSheet` with a `GtkStack` that switches between five states: Idle ("Synced X ago"), Sync ("Syncing..."), Thumbnails ("Thumbnails X/Y"), Upload (expandable with progress bar), and Complete ("Upload Complete"). Priority-based: upload > sync > thumbnails > idle. See `docs/design-sidebar-status-bar.md`.

Import button and hamburger menu live in the **sidebar header** — content headers have view-specific controls only (zoom, selection).

### Live-update pattern (watch channels)

Settings that affect background tasks (sync interval, cache limit) use `tokio::sync::watch` channels for live updates from the Preferences dialog without restarting the app:

1. GSettings value read on GTK thread at startup → initial `watch::channel` value
2. Background task reads via `borrow_and_update()` each cycle (must use `_and_update` to avoid re-triggering `changed()`)
3. Preferences dialog calls `lib.set_sync_interval()` / `lib.set_cache_limit()` which sends to the watch
4. `tokio::select!` on sleep + `changed()` wakes the task immediately on value change
5. `LibraryStorage` trait has default no-op methods — only Immich backend implements them

### View routing and action groups

All content views are GObject widget subclasses registered directly with the `ContentCoordinator` (a thin `GtkStack` wrapper). Views self-install their action groups via `widget.insert_action_group("view", ...)` — GTK's action resolution walks up the widget tree to find them. No trait abstraction needed; this follows the standard GNOME pattern (Fractal, GNOME Settings).

### Event bus and command dispatch

`src/event_bus.rs` — centralised push-based event delivery using `glib::idle_add_once`. Components subscribe in their own constructors; parents do assembly only.

- **`AppEvent`** (`app_event.rs`) — command variants (`*Requested`) and result variants (`*Changed`, `Trashed`, etc.)
- **`EventSender`** — `Send + Clone` wrapper around `mpsc::Sender`. Safe to call from Tokio threads.
- **`CommandDispatcher`** (`commands/dispatcher.rs`) — subscribes to the bus, routes `*Requested` events to `CommandHandler` impls on the Tokio runtime. Each handler emits result events or `AppEvent::Error` on failure.
- **Error toasts** — a subscriber in `application.rs` listens for `AppEvent::Error` and shows an `AdwToast` via `WidgetExt::activate_action("win.show-toast", ...)`. **Important:** Use `WidgetExt::activate_action` (not `ActionGroupExt::activate_action`) — `ActionGroupExt` does not resolve the `win.` prefix.
- **Blocking errors** — library open/create failures show `AdwAlertDialog` with error details and recovery options.

### Viewer headerbar and overflow menu

Photo and video viewers use a clean headerbar: `[★] [ℹ] [✏] [⋮]`. The overflow menu (⋮) is a manual `gtk::Popover` with icon+label buttons (not `GMenuModel` — GTK4's `PopoverMenu` does not render icons from `GMenuModel` attributes). Items: Add to album, Share, Export original, Set as wallpaper (photo only), Show in Files, Delete (destructive, separated). Shared builder: `build_viewer_menu_popover()` and `find_menu_button()` in `viewer/menu.rs` (re-exported from `viewer.rs` for `video_viewer.rs`).

### Album picker dialog

`src/ui/album_picker_dialog.rs` — full `adw::Dialog` replacing the old popover. Architecture: async data fetch → `AlbumPickerData` (plain structs) → dialog construction → `AppEvent` bus commands. The dialog never imports `Library`. Features: search with Pango bold highlighting, cover thumbnails (pre-decoded on Tokio), "Already added" pills, inline "New album…" creation flow, empty state.

### Icons

Use only icons confirmed to exist in the Adwaita icon theme. Common ones: `object-select-symbolic` (checkmark), `view-refresh-symbolic` (sync), `view-conceal-symbolic` (eye-slash/hidden), `folder-download-symbolic`, `go-up-symbolic`, `document-send-symbolic`. Check with `find /usr/share/icons/Adwaita -name "icon-name.svg"` before using.

## Development Workflow

When fixing compilation errors, always run a full build (`cargo build` or `make build`) to verify the fix compiles before moving on. Do not assume a fix works without compiling.

## Tracing / logging

All log output uses the `tracing` crate — never `println!` or `eprintln!`.

- `tracing_subscriber` is initialised in `main()` with `EnvFilter::from_default_env()`; default level is `info`, control verbosity with `RUST_LOG=moments=debug`
- Use `#[instrument]` on every function worth timing (async backend methods, factory calls, bundle open/create)
- Use `#[instrument(skip(field))]` to omit large or sensitive parameters from spans
- Level guidance: `error!` — unrecoverable; `warn!` — degraded but continuing; `info!` — lifecycle milestones (start, open, close); `debug!` — per-operation detail

## Code conventions

- Use `mod.rs` for modules with children; co-locate `.blp` Blueprint templates with their Rust code inside the directory (e.g. `src/ui/photo_grid/mod.rs` + `photo_grid.blp` + `cell.blp`)
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

### Blueprint templates

Most widgets use Blueprint (`.blp`) declarative templates compiled to GTK XML. Existing templates: `window.blp`, `setup_window.blp` + pages, `import_dialog.blp`, `shortcuts-dialog.blp`, `viewer.blp`, `video_viewer.blp`, `edit_panel.blp`, `photo_grid.blp`, `collection_grid.blp`, `album_grid.blp`. New widgets should use Blueprint for static layout and keep dynamic construction in Rust. See `docs/design-gobject-blueprint-refactor.md` for the full pattern and lessons learned.


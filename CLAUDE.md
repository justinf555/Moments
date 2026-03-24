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

The Flatpak manifest is `io.github.justinf555.Moments.json`. It pulls source from the local git repo (`file:///home/justin/Projects/Moments`, branch `main`), so **changes must be committed before rebuilding**.

Instruct the user to test via GNOME Builder or `make run` — do not attempt to run the app binary directly.

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
    storage.rs         — LibraryStorage async trait (open/close)
    media.rs           — MediaId, MediaItem, MediaFilter, LibraryMedia trait
    album.rs           — AlbumId, Album, LibraryAlbums trait
    import.rs          — LibraryImport trait
    thumbnail.rs       — LibraryThumbnail trait, sharded path helpers
    viewer.rs          — LibraryViewer trait (original file access)
    error.rs           — LibraryError enum (thiserror-based)
    event.rs           — LibraryEvent enum (channel-based backend → GTK communication)
    db.rs              — Database struct (sqlx::SqlitePool), all SQL queries
    db/migrations/     — Numbered SQL migrations (001–009)
    bundle.rs          — Library bundle on disk (manifest, paths)
    config.rs          — LibraryConfig enum (Local / Immich)
    factory.rs         — LibraryFactory (creates backends by config type)
    immich_client.rs   — ImmichClient (HTTP client for Immich API)
    keyring.rs         — GNOME Keyring integration (session token storage)
    sync.rs            — SyncManager + ThumbnailDownloader (Immich background sync)
    format/            — Format detection (magic bytes, standard/raw/video handlers)
    providers/
      local.rs         — LocalLibrary (local filesystem backend)
      immich.rs        — ImmichLibrary (Immich server backend)
  ui/
    window.rs          — MomentsWindow; wires sidebar, coordinator, views
    sidebar.rs         — MomentsSidebar with dynamic album section
    sidebar/
      route.rs         — TOP_ROUTES / BOTTOM_ROUTES definitions
      row.rs           — MomentsSidebarRow widget
    coordinator.rs     — ContentCoordinator (stack-based view routing)
    model_registry.rs  — ModelRegistry (broadcasts events to all grid models)
    photo_grid.rs      — PhotoGridView (header bar, actions, viewer integration)
    photo_grid/
      model.rs         — PhotoGridModel (pagination, filtering, incremental updates)
      factory.rs       — Cell factory (bind/unbind with texture management)
      cell.rs          — PhotoGridCell widget (placeholder → thumbnail → star)
      item.rs          — MediaItemObject (GObject wrapper for grid items)
    viewer.rs          — PhotoViewer (full-res image display)
    video_viewer.rs    — VideoViewer (GStreamer playback)
    album_dialogs.rs   — Create/rename/delete album dialogs
    import_dialog.rs   — Import progress dialog
    setup_window/      — Setup wizard (backend picker, local setup, Immich setup)
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

Results flow back from Tokio → GTK via `Sender<LibraryEvent>` (a `std::sync::mpsc` channel, which is `Send`).

### Library abstraction layer

`Library` (in `library.rs`) is a blanket-impl composition of feature sub-traits: `LibraryStorage + LibraryImport + LibraryMedia + LibraryThumbnail + LibraryViewer + LibraryAlbums`. All backend work runs on the Tokio executor. `LibraryStorage::open()` receives a `tokio::runtime::Handle` which is stored for the backend's lifetime.

Two backends exist:
- **`LocalLibrary`** (`providers/local.rs`) — stores originals on disk, generates thumbnails locally
- **`ImmichLibrary`** (`providers/immich.rs`) — syncs with an Immich server via `POST /sync/stream`, caches everything locally in the same SQLite schema. Background sync polls every 30s with a thumbnail download worker pool. See `docs/design-immich-backend.md` for the full design.

### Database

`src/library/db.rs` — backend-agnostic `Database` struct wrapping an `sqlx::SqlitePool`. Used by all backends that need persistence. Migrations live at `src/library/db/migrations/` and are embedded via `sqlx::migrate!()`. **Every schema change must be a numbered migration — no ad-hoc `CREATE TABLE IF NOT EXISTS` in code.**

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

## Tracing / logging

All log output uses the `tracing` crate — never `println!` or `eprintln!`.

- `tracing_subscriber` is initialised in `main()` with `EnvFilter::from_default_env()`; control verbosity with `RUST_LOG=moments=debug`
- Use `#[instrument]` on every function worth timing (async backend methods, factory calls, bundle open/create)
- Use `#[instrument(skip(field))]` to omit large or sensitive parameters from spans
- Level guidance: `error!` — unrecoverable; `warn!` — degraded but continuing; `info!` — lifecycle milestones (start, open, close); `debug!` — per-operation detail

## Code conventions

- Use Rust 2018+ module naming: place submodules in `src/foo/bar.rs`, never `src/foo/bar/mod.rs`
- Prefer many small, focused files over large ones
- Every feature must be developed on a dedicated git branch — never commit directly to `main` without branching first
- GTK dependency versions are pinned together — keep `gtk4` and `libadwaita` version-aligned when upgrading

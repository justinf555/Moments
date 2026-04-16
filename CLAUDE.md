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

### Layered architecture

The codebase follows a strict layered architecture:

```
GTK Widgets (ui/)
    ↕ GObject properties, signals, ListStore models
Client Layer (client/)
    ↕ async calls on Tokio, results via GObject property notifications
Library Services (library/)
    ↕ service methods, model types
Repository Layer (library/*/repository.rs)
    ↕ SQL queries via sqlx
Database (library/db/)
```

Each library feature (media, album, faces, editing, thumbnail, metadata) is split into:
- **`model.rs`** — domain types (newtypes, enums, records)
- **`repository.rs`** — database queries (thin sqlx wrappers)
- **`service.rs`** — business logic composing repository calls

### Top-level modules

```
src/
  main.rs              — Entry point: gettext, GResources, MomentsApplication
  config.rs            — Compile-time constants (VERSION, PKGDATADIR, etc.)
  application/         — MomentsApplication (adw::Application subclass)
  app_event/           — AppEvent enum (commands + results)
  event_bus/           — EventBus (push-based fan-out via glib::idle_add_once)
  library/             — Core domain: Library struct + feature services
  client/              — GObject bridge layer (MediaClient, AlbumClient, etc.)
  renderer/            — RenderPipeline (decode → orient → resize → edits)
  importer/            — Import pipeline (discovery → hash → metadata → persist)
  sync/                — Bidirectional Immich sync engine
  tasks/               — Background tasks (trash auto-purge)
  ui/                  — All GTK widgets
```

### Library (service layer)

`Library` (`library/mod.rs`) is a **concrete struct** composing six feature services:

```rust
pub struct Library {
    albums: AlbumService,
    faces: FacesService,
    editing: EditingService,
    media: MediaService,
    metadata: MetadataService,
    thumbnails: ThumbnailService,
}
```

Constructed via `Library::open(bundle, mode, db, recorder, resolver)`. Access features through service accessors: `.media()`, `.albums()`, `.faces()`, `.editing()`, `.metadata()`, `.thumbnails()`.

Key abstractions injected at construction:
- **`MutationRecorder`** (`library/recorder.rs`) — trait for recording state changes. `NoOpRecorder` for local backend; `QueueWriterOutbox` for Immich (writes to outbox table for push sync).
- **`OriginalResolver`** (`library/resolver.rs`) — trait for accessing original files. `LocalResolver` for local filesystem; `CachedResolver` for Immich (downloads from server to cache).

### Client layer (GObject bridge)

`src/client/` — GObject singletons that bridge Library services to GTK widgets:

- **`MediaClient`** — media queries, filtering, ListStore model factory
- **`AlbumClient`** — album CRUD, album picker data
- **`PeopleClient`** — person queries, visibility management
- **`ImportClient`** — import pipeline orchestration, progress tracking

Clients are GObject subclasses with property notifications. They create and manage `ListStore` models via weak refs (factory pattern). GTK widgets bind to client properties and models — they never import `Library` directly.

### Renderer

`src/renderer/` — stateless `RenderPipeline` with step modules:

- `decode.rs` — format detection and decoding (delegates to format registry)
- `orientation.rs` — EXIF orientation correction
- `resize.rs` — thumbnail sizing
- `edits.rs` — non-destructive edit application
- `output.rs` — RGBA/WebP conversion
- `format/` — `FormatRegistry` with handlers: `standard.rs` (JPEG/PNG/WebP), `raw.rs` (RAW via libraw), `video.rs` (video frame extraction)

The pipeline builds its own `FormatRegistry` internally — format handlers are private. All consumers (thumbnail generation, viewer full-res, edit sessions) go through `RenderPipeline`.

### Importer

`src/importer/` — standalone import pipeline built with a builder pattern:

`ImportPipelineBuilder` → `ImportPipeline::run()` with steps: discovery → filter → hash → metadata → persistence → thumbnail. Emits `ImportProgress` updates and returns `ImportSummary`. Independent of the Library trait system.

### Sync engine

`src/sync/` — bidirectional Immich sync:

- `outbox/` — mutation recording: `MutationRecorder` trait, `QueueWriterOutbox` (writes to DB), `NoOpRecorder` (local backend)
- `providers/immich/` — Immich-specific sync:
  - `client.rs` — HTTP client for Immich API (`POST /sync/stream`)
  - `pull.rs` — inbound sync (server → local)
  - `push.rs` — outbound sync (local → server, drains outbox)
  - `handlers/` — per-entity sync handlers (asset, album, person, face, exif)
  - `resolver.rs` — `CachedResolver` for downloading originals

The sync engine is spawned from `application/mod.rs` via `SyncHandle::start()`, not from Library.

### Database

`src/library/db/mod.rs` — backend-agnostic `Database` struct wrapping `sqlx::SqlitePool`. Created with `Database::new()`, connected with `db.open(&path)`. Migrations live at `src/library/db/migrations/` and are embedded via `sqlx::migrate!()`. **Every schema change must be a numbered migration — no ad-hoc `CREATE TABLE IF NOT EXISTS` in code.**

After any schema change, regenerate the offline query snapshot:
```bash
cargo sqlx database create
cargo sqlx migrate run
cargo sqlx prepare    # regenerates .sqlx/ — commit this directory
```

CI sets `SQLX_OFFLINE=true` and uses the committed `.sqlx/` snapshot.

### MediaId

`MediaId` is a UUID v4 stored as a 32-char lowercase hex string (no dashes). Generated via `MediaId::generate()` for local imports; loaded from DB or Immich sync via `MediaId::new()`. Content-based deduplication uses the separate `content_hash` field, not the ID.

### Two-executor model

- **GTK executor** (`glib::MainContext`) — UI thread only. Widget updates, signal handlers, client calls via `glib::MainContext::default().spawn_local()`.
- **Library executor** (Tokio runtime) — all backend I/O: database, file ops, HTTP. Created in `main()`, shared via `tokio::runtime::Handle` on `MomentsApplication`.

Results flow from Tokio → GTK via the event bus (`glib::idle_add_once`) or GObject property notifications on clients.

### Application singleton pattern

Access shared state via `MomentsApplication::default()` with typed accessors: `tokio_handle()`, `library()`, `media_client()`, `album_client()`, `people_client()`, `import_client()`, `render_pipeline()`. Don't walk the widget tree with `.root().application()`.

### Event bus and command dispatch

`src/event_bus/mod.rs` — centralised push-based event delivery using `glib::idle_add_once`. Components subscribe in their own constructors; parents do assembly only.

- **`AppEvent`** (`app_event/mod.rs`) — command variants (`*Requested`) and result variants (`*Changed`, `Trashed`, etc.)
- **`EventSender`** — `Send + Clone` wrapper around `mpsc::Sender`. Safe to call from Tokio threads.
- **`CommandDispatcher`** (`library/commands/mod.rs`) — subscribes to the bus, routes `*Requested` events to `CommandHandler` impls on the Tokio runtime.
- **Error toasts** — `AppEvent::Error` → `AdwToast` via `WidgetExt::activate_action("win.show-toast", ...)`. Use `WidgetExt` not `ActionGroupExt`.

### GTK/GObject subclassing pattern

All GObject types follow the split `imp` module pattern:
- The inner `mod imp` struct holds state and implements GObject trait impls
- The outer `glib::wrapper!` macro creates the public Rust type
- UI templates are declared with `#[template(resource = "...")]` and bound in `class_init`/`instance_init`

### Sidebar

The sidebar uses `AdwSidebar` with routes defined in `sidebar/route.rs`. People route is hidden for the Local backend (no face detection). Pinned albums are added dynamically as a separate `AdwSidebarSection`.

The sidebar bottom bar is a persistent `AdwBottomSheet` with a `GtkStack` that switches between five states: Idle, Sync, Thumbnails, Upload, and Complete. Priority-based: upload > sync > thumbnails > idle. See `docs/design-sidebar-status-bar.md`.

Import button and hamburger menu live in the **sidebar header** — content headers have view-specific controls only (zoom, selection).

### Live-update pattern (watch channels)

Settings that affect background tasks (sync interval, cache limit) use `tokio::sync::watch` channels:

1. GSettings value → initial `watch::channel` value
2. Background task reads via `borrow_and_update()` each cycle
3. Preferences dialog sends new value via setter
4. `tokio::select!` on sleep + `changed()` wakes task immediately

### View routing and action groups

Content views are GObject widget subclasses registered with `ContentCoordinator` (a `GtkStack` wrapper). Views self-install action groups via `widget.insert_action_group("view", ...)` — GTK's action resolution walks the widget tree.

### Viewer headerbar and overflow menu

Photo and video viewers: `[★] [ℹ] [✏] [⋮]`. The overflow menu uses a manual `gtk::Popover` with icon+label buttons (not `GMenuModel`). Shared builder in `viewer/menu.rs`.

### Album picker dialog

`src/ui/album_picker_dialog/` — `adw::Dialog` with search, cover thumbnails, "Already added" pills, inline creation. Architecture: async data fetch → `AlbumPickerData` (plain structs) → dialog → `AppEvent` bus commands. Never imports `Library`.

### Icons

Use only icons confirmed in the Adwaita icon theme. Check with `find /usr/share/icons/Adwaita -name "icon-name.svg"` before using.

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

Most widgets use Blueprint (`.blp`) declarative templates compiled to GTK XML. New widgets should use Blueprint for static layout and keep dynamic construction in Rust. See `docs/design-gobject-blueprint-refactor.md` for the full pattern and lessons learned.

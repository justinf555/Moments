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
- **`async-trait`** for async trait definitions (no Tokio runtime yet — async will bridge to GTK's main loop via `glib::idle_add`)
- **`thiserror`** for error types

### Module structure

```
src/
  main.rs          — Entry point: sets up gettext, loads GResources, creates MomentsApplication
  application.rs   — MomentsApplication (adw::Application subclass); registers GActions
  window.rs        — MomentsWindow (adw::ApplicationWindow subclass); binds UI template
  config.rs        — Compile-time constants (VERSION, PKGDATADIR, etc.)
  library.rs       — Library trait (top-level photo library abstraction)
  library/
    storage.rs     — LibraryStorage async trait (open/close a library bundle on disk)
    error.rs       — LibraryError enum (thiserror-based)
```

### GTK/GObject subclassing pattern

All GObject types follow the split `imp` module pattern:
- The inner `mod imp` struct holds state and implements GObject trait impls
- The outer `glib::wrapper!` macro creates the public Rust type
- UI templates are declared with `#[template(resource = "...")]` and bound in `class_init`/`instance_init`

### Library abstraction layer

`Library` (in `library.rs`) and `LibraryStorage` (in `library/storage.rs`) are async traits designed to be implemented by multiple backends (local filesystem, Immich, etc.). `LibraryStorage` handles the raw persistence layer; `Library` will sit above it. All backend I/O must be async and run off the GTK main thread, bridging back via `glib::idle_add`.

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

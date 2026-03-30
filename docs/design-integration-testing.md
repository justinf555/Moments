# Design: Integration & App Testing (#TBD)

**Status:** Proposed
**Date:** 2026-03-30

## Overview

Establish a three-tier headless test suite covering widget integration tests, end-to-end app smoke tests, and event bus flow tests. All tiers run in CI without a physical display using a headless Wayland compositor (`mutter --headless`) with `GSK_RENDERER=cairo` fallback rendering. No X11 dependency — compatible with GNOME 48+ (X11-free sessions) and future GNOME releases.

## Problem

Moments has 204 unit tests with strong library-layer coverage (database, format detection, editing, sync, import). However, there are **zero tests** for:

- **Widget behaviour** — selection mode, headerbar transforms, zoom controls, action bar context switching
- **App lifecycle** — startup, library open, shutdown, error recovery
- **Cross-component flows** — button click → library call → model update → UI reaction

These gaps mean that UI regressions, wiring errors, and integration bugs are only caught by manual testing. As the codebase grows (event bus migration, GNOME Circle features), the risk of shipping broken flows increases.

### What other GNOME apps do

| App | Approach |
|-----|----------|
| **Fractal** (Rust, GTK4) | Pure Rust unit tests only. No display, no widget tests. Logic separated from GObject wrappers. |
| **Loupe** (Rust, GTK4) | `cargo test` wrapped in `xvfb-run`. Some tests exercise widgets. |
| **Nautilus** (C, GTK4) | Splits into `displayless/` and `display/` test suites. Most thorough approach in GNOME. |
| **gtk4-rs** (Rust bindings) | `xvfb-run --auto-servernum cargo test -- --test-threads=1` with `GSK_RENDERER=cairo`. |

Moments should follow the Nautilus pattern — explicit separation of displayless (library) and display (widget/app) tests — implemented in Rust with the gtk4-rs testing patterns.

## Constraints

### GTK4 headless testing rules

1. **Single-threaded** — GTK is not thread-safe. All widget tests must run with `--test-threads=1`.
2. **`GSK_RENDERER=cairo`** — GTK4's default OpenGL renderer doesn't work without a GPU. Cairo software rendering is required.
3. **Headless compositor** — GTK needs a display server even for headless tests. Two options exist:
   - **`mutter --headless`** (preferred) — headless Wayland compositor, no X11 dependency. This is what Nautilus and GNOME CI use. Works on GNOME 48+ where X11 is dropped.
   - **`xvfb-run`** (fallback) — virtual X11 framebuffer. Still works on systems with X11 libs installed, but not future-proof for GNOME 50+.
4. **`#[gtk::test]`** — The `gtk4_macros` test attribute auto-initialises GTK and provides async support. Replaces `#[test]` for any test that touches widgets.
5. **No click simulation** — There is no GTK4 equivalent of Selenium or Playwright. Tests call widget methods programmatically and assert on resulting state.
6. **`dbus-run-session`** — Some GTK features (file dialogs, notifications) require a session bus. Wrap test execution in `dbus-run-session` to provide one.

### Headless display: mutter vs xvfb

```
┌─────────────────────────┬──────────────────────┬───────────────────────┐
│                         │ mutter --headless    │ xvfb-run              │
├─────────────────────────┼──────────────────────┼───────────────────────┤
│ Display protocol        │ Wayland              │ X11                   │
│ GNOME 48+ compatible    │ Yes                  │ Yes (with X11 libs)   │
│ GNOME 50+ compatible    │ Yes                  │ No (X11 removed)      │
│ GPU required            │ No                   │ No                    │
│ SSH / headless server   │ Yes                  │ Yes                   │
│ CI container support    │ Yes (Fedora 43+)     │ Yes (any Linux)       │
│ Package                 │ mutter (Fedora)      │ xorg-x11-server-Xvfb │
│ Invocation              │ mutter --headless    │ xvfb-run -a           │
│                         │   --wayland --no-x11 │   -s "-screen 0       │
│                         │   --virtual-monitor  │      1024x768x24"    │
│                         │   1024x768 -- CMD    │   CMD                 │
└─────────────────────────┴──────────────────────┴───────────────────────┘
```

**Decision:** Use `mutter --headless` as the primary approach. This aligns with GNOME's direction, avoids X11 dependencies, and will remain supported as GNOME drops X11. The CI container (Fedora 43) already has mutter available.

### Available GTK4 test utilities

```rust
// Widget rendering synchronisation
gtk::test_widget_wait_for_draw(&widget);

// Accessibility assertions (verify GNOME Circle a11y requirements)
gtk::test_accessible_has_role(&widget, gtk::AccessibleRole::Img);
gtk::test_accessible_has_property(&widget, gtk::AccessibleProperty::Label);
gtk::test_accessible_has_state(&widget, gtk::AccessibleState::Selected);
```

## Test Architecture

### Directory structure

```
tests/
  common/
    mod.rs              — Shared helpers (test library, temp bundles, test images)
  integration/
    mod.rs              — #[cfg(feature = "integration-tests")]
    test_photo_grid.rs  — Selection mode, zoom, headerbar transforms
    test_model_registry.rs — Event broadcast to multiple models (pre-bus)
    test_action_bar.rs  — Context-sensitive buttons per MediaFilter
    test_sidebar.rs     — Route navigation, album section updates
    test_viewer.rs      — Photo viewer navigation, panel toggles
    test_accessibility.rs — AccessibleRole/Property assertions for Circle
  e2e/
    mod.rs              — #[cfg(feature = "integration-tests")]
    test_app_lifecycle.rs — App starts, window appears, shuts down cleanly
    test_local_library.rs — Create bundle → import → grid shows photos
    test_import_flow.rs   — Import dialog → files copied → thumbnails generated
  bus/
    mod.rs              — #[cfg(feature = "integration-tests")]
    test_event_bus.rs   — Subscribe → send → all handlers called
    test_commands.rs    — TrashRequested → library.trash() → Trashed event
    test_translator.rs  — LibraryEvent → AppEvent 1:1 mapping
    test_event_flow.rs  — Full: button → command → handler → model update
```

### Feature flag

Integration tests are gated behind a Cargo feature so `cargo test` (unit tests) remains fast and displayless:

```toml
# Cargo.toml
[features]
editing = []
integration-tests = []

[dev-dependencies]
tempfile = "3"
```

- `cargo test` — runs 204 unit tests, no display needed, fast
- `cargo test --features integration-tests` — runs everything including widget/app tests, requires `xvfb-run`

### Test helpers (`tests/common/mod.rs`)

Shared infrastructure for all integration tests:

```rust
use std::path::PathBuf;
use tempfile::TempDir;

/// Creates a temporary library bundle with test images.
/// Returns (TempDir, PathBuf) — hold TempDir to keep the directory alive.
pub fn create_test_bundle() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let bundle_path = dir.path().join("test-library");
    // Create bundle manifest, originals dir, thumbnails dir
    // Copy test fixtures from tests/fixtures/
    (dir, bundle_path)
}

/// Copies test JPEG/PNG files into the bundle's originals directory.
pub fn add_test_images(bundle_path: &Path, count: usize) { /* ... */ }

/// Creates a mock Library implementation for isolated widget testing.
/// Returns predictable results without touching the filesystem.
pub fn mock_library() -> Arc<dyn Library> { /* ... */ }

/// Waits for the GTK main loop to process all pending events.
pub fn flush_events() {
    while glib::MainContext::default().iteration(false) {}
}

/// Waits for a condition to become true, processing GTK events.
/// Panics after timeout_ms if the condition never becomes true.
pub fn wait_until(condition: impl Fn() -> bool, timeout_ms: u64) {
    let start = std::time::Instant::now();
    while !condition() {
        if start.elapsed().as_millis() > timeout_ms as u128 {
            panic!("wait_until timed out after {timeout_ms}ms");
        }
        glib::MainContext::default().iteration(false);
    }
}
```

## Tier 1: Widget Integration Tests

Test individual GTK components with their real widget tree, but without the full application stack. Components are instantiated directly, methods called programmatically, state asserted.

### Pattern

```rust
// tests/integration/test_photo_grid.rs
#[cfg(feature = "integration-tests")]
mod tests {
    use super::*;

    #[gtk::test]
    fn enter_selection_mode_transforms_headerbar() {
        let bus = EventBus::new();
        let grid = PhotoGridView::new(/* mock library, tokio, bus */);

        // Precondition: normal mode
        assert!(grid.imp().zoom_controls.is_visible());
        assert!(!grid.imp().cancel_button.is_visible());

        // Act: enter selection mode
        grid.activate_action("view.enter-selection", None);
        flush_events();

        // Assert: headerbar transformed
        assert!(!grid.imp().zoom_controls.is_visible());
        assert!(grid.imp().cancel_button.is_visible());
        assert!(grid.imp().selection_count_label.is_visible());
    }

    #[gtk::test]
    fn exit_selection_mode_on_escape() {
        let bus = EventBus::new();
        let grid = PhotoGridView::new(/* ... */);

        grid.activate_action("view.enter-selection", None);
        flush_events();
        assert!(grid.imp().cancel_button.is_visible());

        grid.activate_action("view.exit-selection", None);
        flush_events();

        assert!(grid.imp().zoom_controls.is_visible());
        assert!(!grid.imp().cancel_button.is_visible());
    }
}
```

### Key test cases

| Component | Test | Asserts |
|-----------|------|---------|
| `PhotoGridView` | Enter/exit selection mode | Headerbar widget visibility swaps |
| `PhotoGridView` | Zoom in/out | Zoom level changes, grid item size updates |
| `PhotoGridView` | Auto-exit selection on zero items | Selection mode exits when last item deselected |
| `ActionBarFactory` | `build_for_filter(Standard)` | Favourite + Album + Trash buttons present |
| `ActionBarFactory` | `build_for_filter(Trashed)` | Restore + Delete buttons present |
| `ActionBarFactory` | `build_for_filter(Album)` | Favourite + Remove + Trash buttons present |
| `MomentsSidebar` | Route selection | Correct route emitted on row click |
| `MomentsSidebar` | Album added | New album row appears in sidebar |
| `PhotoViewer` | Info panel toggle | Panel visibility toggles |
| `PhotoGridCell` | Selection mode checkbox | Checkbox visible/hidden with mode |
| `PreferencesDialog` | Format functions | Already tested — verify widget binding |

### Accessibility tests

GNOME Circle requires proper accessibility. Use GTK4's built-in assertions:

```rust
// tests/integration/test_accessibility.rs
#[gtk::test]
fn grid_cells_have_img_role() {
    let cell = PhotoGridCell::new();
    assert!(gtk::test_accessible_has_role(
        &cell,
        gtk::AccessibleRole::Img
    ));
}

#[gtk::test]
fn sidebar_rows_have_labels() {
    let row = MomentsSidebarRow::new("Photos", "image-x-generic-symbolic");
    assert!(gtk::test_accessible_has_property(
        &row,
        gtk::AccessibleProperty::Label
    ));
}

#[gtk::test]
fn action_bar_buttons_have_labels() {
    let bus = EventBus::new();
    let sel = gtk::MultiSelection::new(None::<gtk::gio::ListStore>);
    let buttons = ActionBarFactory::build_for_filter(&MediaFilter::default(), &sel, &bus);
    for button in buttons.iter() {
        assert!(gtk::test_accessible_has_property(
            button,
            gtk::AccessibleProperty::Label
        ));
    }
}
```

## Tier 2: End-to-End App Smoke Tests

Test the full application stack: `MomentsApplication` → `MomentsWindow` → views → library. Uses a real SQLite database in a temp directory with test image fixtures.

### Pattern

```rust
// tests/e2e/test_app_lifecycle.rs
#[cfg(feature = "integration-tests")]
mod tests {
    #[gtk::test]
    fn app_starts_and_shows_window() {
        let app = MomentsApplication::new();
        // Simulate activation without running the full main loop
        app.activate();
        flush_events();

        let window = app.active_window();
        assert!(window.is_some());
        assert!(window.unwrap().is_visible());
    }

    #[gtk::test]
    fn app_shuts_down_cleanly() {
        let app = MomentsApplication::new();
        app.activate();
        flush_events();

        app.quit();
        flush_events();
        // No panic, no leaked resources
    }
}
```

```rust
// tests/e2e/test_local_library.rs
#[cfg(feature = "integration-tests")]
mod tests {
    #[gtk::test]
    async fn import_photos_appear_in_grid() {
        let (_dir, bundle_path) = create_test_bundle();
        add_test_images(&bundle_path, 5);

        let app = MomentsApplication::new();
        app.open_library(&bundle_path).await;
        flush_events();

        // Import test images
        app.import_from(&bundle_path.join("originals")).await;
        flush_events();

        // Grid model should contain the imported items
        let model = app.window().photo_grid().model();
        assert_eq!(model.n_items(), 5);
    }
}
```

### Key test cases

| Test | Flow | Asserts |
|------|------|---------|
| App starts | `new()` → `activate()` | Window visible, no crash |
| App shuts down | `activate()` → `quit()` | Clean shutdown, no panic |
| Local library open | Create bundle → open | Library connected, grid model created |
| Import flow | Add images → import | Grid model populated, thumbnails generated |
| Navigation | Click sidebar route | Correct view displayed in content area |
| Empty library | Open empty bundle | Empty state view shown |

## Tier 3: Event Bus Integration Tests

Test the event bus infrastructure and command → result flows. The bus mechanics (`EventBus`, `CommandDispatcher`, event translator) can be tested **without a display** using `glib::init()` only. Full flow tests (button → model update) need xvfb.

### Displayless bus tests

```rust
// tests/bus/test_event_bus.rs — no display needed, glib::init() only
#[cfg(feature = "integration-tests")]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn subscribe_receives_events() {
        glib::init();
        let bus = EventBus::new();

        let received = Rc::new(Cell::new(false));
        let r = Rc::clone(&received);
        bus.subscribe(move |event| {
            if matches!(event, AppEvent::SyncStarted) {
                r.set(true);
            }
        });

        bus.sender().send(AppEvent::SyncStarted).unwrap();
        flush_events();

        assert!(received.get());
    }

    #[test]
    fn multiple_subscribers_all_receive() {
        glib::init();
        let bus = EventBus::new();

        let count = Rc::new(Cell::new(0));
        for _ in 0..3 {
            let c = Rc::clone(&count);
            bus.subscribe(move |event| {
                if matches!(event, AppEvent::SyncStarted) {
                    c.set(c.get() + 1);
                }
            });
        }

        bus.sender().send(AppEvent::SyncStarted).unwrap();
        flush_events();

        assert_eq!(count.get(), 3);
    }

    #[test]
    fn unrelated_events_ignored() {
        glib::init();
        let bus = EventBus::new();

        let received = Rc::new(Cell::new(false));
        let r = Rc::clone(&received);
        bus.subscribe(move |event| {
            if matches!(event, AppEvent::SyncStarted) {
                r.set(true);
            }
        });

        bus.sender().send(AppEvent::SyncComplete {
            assets: 0, people: 0, faces: 0, errors: 0,
        }).unwrap();
        flush_events();

        assert!(!received.get());
    }
}
```

### Command dispatch tests

```rust
// tests/bus/test_commands.rs
#[cfg(feature = "integration-tests")]
mod tests {
    #[tokio::test]
    async fn trash_command_calls_library_and_emits_result() {
        glib::init();
        let bus = EventBus::new();
        let library = mock_library();  // mock returns Ok(()) for trash()

        let received = Rc::new(Cell::new(false));
        let r = Rc::clone(&received);
        bus.subscribe(move |event| {
            if matches!(event, AppEvent::Trashed { .. }) {
                r.set(true);
            }
        });

        let ids = vec![MediaId::new("abc123")];
        let handler = TrashCommand;
        handler.execute(
            AppEvent::TrashRequested { ids },
            &library,
            &bus.sender(),
        ).await;
        flush_events();

        assert!(received.get());
    }

    #[tokio::test]
    async fn trash_command_emits_error_on_failure() {
        glib::init();
        let bus = EventBus::new();
        let library = failing_mock_library();  // mock returns Err for trash()

        let error_msg = Rc::new(RefCell::new(String::new()));
        let e = Rc::clone(&error_msg);
        bus.subscribe(move |event| {
            if let AppEvent::Error(msg) = event {
                *e.borrow_mut() = msg.clone();
            }
        });

        let handler = TrashCommand;
        handler.execute(
            AppEvent::TrashRequested { ids: vec![MediaId::new("abc123")] },
            &library,
            &bus.sender(),
        ).await;
        flush_events();

        assert!(!error_msg.borrow().is_empty());
    }
}
```

### Event translator tests

```rust
// tests/bus/test_translator.rs
#[cfg(feature = "integration-tests")]
mod tests {
    #[test]
    fn library_thumbnail_ready_translates_to_app_event() {
        glib::init();
        let bus = EventBus::new();

        let received_id = Rc::new(RefCell::new(None));
        let r = Rc::clone(&received_id);
        bus.subscribe(move |event| {
            if let AppEvent::ThumbnailReady { media_id } = event {
                *r.borrow_mut() = Some(media_id.clone());
            }
        });

        let (lib_tx, lib_rx) = glib::MainContext::channel::<LibraryEvent>(
            glib::Priority::DEFAULT
        );
        start_event_translator(lib_rx, &bus);

        let id = MediaId::new("test-media-123");
        lib_tx.send(LibraryEvent::ThumbnailReady {
            media_id: id.clone()
        }).unwrap();
        flush_events();

        assert_eq!(received_id.borrow().as_ref(), Some(&id));
    }
}
```

### Full flow tests (need headless compositor)

```rust
// tests/bus/test_event_flow.rs
#[cfg(feature = "integration-tests")]
mod tests {
    #[gtk::test]
    async fn trash_button_removes_items_from_grid() {
        let bus = EventBus::new();
        let library = mock_library();
        let tokio = tokio::runtime::Handle::current();

        // Set up the full pipeline
        let _dispatcher = CommandDispatcher::new(
            Arc::clone(&library), tokio, &bus
        );

        let grid = PhotoGridView::new(/* library, tokio, bus */);
        // Populate grid with test items
        grid.model().append_test_items(3);
        assert_eq!(grid.model().n_items(), 3);

        // Simulate: select all → click trash
        grid.selection().select_all();
        let ids = collect_selected_ids(&grid.selection());
        bus.sender().send(AppEvent::TrashRequested { ids }).unwrap();

        // Wait for async command handler + model update
        wait_until(|| grid.model().n_items() == 0, 2000);

        assert_eq!(grid.model().n_items(), 0);
    }
}
```

## CI Configuration

### Updated workflow

```yaml
# .github/workflows/ci.yml
name: CI

on:
  pull_request:
  push:
    branches: [main]

jobs:
  unit-tests:
    name: Unit tests
    runs-on: ubuntu-latest
    container:
      image: fedora:43
    env:
      SQLX_OFFLINE: "true"
    steps:
      - uses: actions/checkout@v4

      - name: Install system dependencies
        run: |
          dnf install -y \
            cargo \
            gtk4-devel \
            libadwaita-devel \
            gettext-devel \
            libheif-devel \
            gstreamer1-devel \
            gstreamer1-plugins-base-devel \
            libsecret-devel \
            pkg-config

      - name: Configure sccache
        uses: mozilla-actions/sccache-action@v0.0.7

      - name: Cache cargo registry
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
          key: fedora43-cargo-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: fedora43-cargo-

      - name: Generate config.rs stub
        run: |
          cat > src/config.rs <<'EOF'
          pub static VERSION: &str = env!("CARGO_PKG_VERSION");
          pub static GETTEXT_PACKAGE: &str = "moments";
          pub static LOCALEDIR: &str = "/usr/share/locale";
          pub static PKGDATADIR: &str = "/usr/share/moments";
          EOF

      - name: Run unit tests
        run: cargo test

      - name: Show sccache stats
        if: always()
        run: sccache --show-stats

  integration-tests:
    name: Integration tests
    runs-on: ubuntu-latest
    container:
      image: fedora:43
    env:
      SQLX_OFFLINE: "true"
      GSK_RENDERER: "cairo"
      GTK_A11Y: "none"
      GIO_USE_VFS: "local"
    steps:
      - uses: actions/checkout@v4

      - name: Install system dependencies
        run: |
          dnf install -y \
            cargo \
            gtk4-devel \
            libadwaita-devel \
            gettext-devel \
            libheif-devel \
            gstreamer1-devel \
            gstreamer1-plugins-base-devel \
            libsecret-devel \
            pkg-config \
            mutter \
            dbus-x11

      - name: Configure sccache
        uses: mozilla-actions/sccache-action@v0.0.7

      - name: Cache cargo registry
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
          key: fedora43-cargo-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: fedora43-cargo-

      - name: Generate config.rs stub
        run: |
          cat > src/config.rs <<'EOF'
          pub static VERSION: &str = env!("CARGO_PKG_VERSION");
          pub static GETTEXT_PACKAGE: &str = "moments";
          pub static LOCALEDIR: &str = "/usr/share/locale";
          pub static PKGDATADIR: &str = "/usr/share/moments";
          EOF

      - name: Run integration tests
        run: >
          dbus-run-session
          mutter --headless --wayland --no-x11 --virtual-monitor 1024x768 --
          cargo test --features integration-tests -- --test-threads=1

      - name: Show sccache stats
        if: always()
        run: sccache --show-stats
```

### Local development

```bash
# Unit tests only (fast, no display)
cargo test

# Integration tests — on a GNOME desktop (display already available):
GSK_RENDERER=cairo cargo test --features integration-tests -- --test-threads=1

# Integration tests — headless (SSH, CI, no desktop):
dbus-run-session \
  mutter --headless --wayland --no-x11 --virtual-monitor 1024x768 -- \
  cargo test --features integration-tests -- --test-threads=1

# Fallback if mutter is unavailable (X11 systems):
xvfb-run -a -s "-screen 0 1024x768x24" \
  dbus-run-session \
  cargo test --features integration-tests -- --test-threads=1
```

### Makefile targets

```makefile
test:
	cargo test

test-integration:
	GSK_RENDERER=cairo cargo test --features integration-tests -- --test-threads=1

test-integration-headless:
	dbus-run-session \
	  mutter --headless --wayland --no-x11 --virtual-monitor 1024x768 -- \
	  cargo test --features integration-tests -- --test-threads=1

test-all:
	cargo test
	$(MAKE) test-integration
```

## Test Fixtures

Small set of test images stored in `tests/fixtures/`:

```
tests/
  fixtures/
    photo_landscape.jpg   — 640x480 JPEG with EXIF (GPS, camera, date)
    photo_portrait.jpg    — 480x640 JPEG with orientation tag
    photo_no_exif.png     — 640x480 PNG, no metadata
    photo_heif.heif       — 640x480 HEIF (tests libheif path)
    video_short.mp4       — 1-second 320x240 MP4 (tests video thumbnail)
```

Keep fixtures minimal (< 100KB total). They test format handling, not image quality.

## Mock Library

For widget integration tests that don't need a real database, a mock `Library` implementation provides predictable responses:

```rust
// tests/common/mock_library.rs
pub struct MockLibrary {
    trash_result: Mutex<Result<(), LibraryError>>,
    restore_result: Mutex<Result<(), LibraryError>>,
    // ... one field per library method
}

impl MockLibrary {
    pub fn new() -> Self { /* all Ok(()) by default */ }
    pub fn fail_trash(mut self) -> Self {
        *self.trash_result.lock().unwrap() = Err(LibraryError::Internal("mock failure".into()));
        self
    }
}

#[async_trait]
impl LibraryMedia for MockLibrary {
    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.trash_result.lock().unwrap().clone()
    }
    // ... other trait methods return sensible defaults
}
```

This avoids pulling in a mocking framework. The mock is hand-written and project-specific — easy to maintain, no magic.

## Implementation Phases

| Phase | Description | Scope | Display needed |
|-------|-------------|-------|----------------|
| 1 | Infrastructure | Feature flag, CI job, test helpers, mock library, fixtures | No |
| 2 | Event bus tests | `EventBus` subscribe/send, `CommandHandler` dispatch, translator | No (`glib::init()` only) |
| 3 | Widget integration tests | PhotoGridView, ActionBarFactory, Sidebar, accessibility | Yes (mutter --headless) |
| 4 | End-to-end smoke tests | App lifecycle, library open, import flow | Yes (mutter --headless) |
| 5 | Ongoing | New tests alongside every new feature/bug fix | Varies |

**Phase 1–2 can begin immediately** — they don't need a compositor and will validate the event bus implementation (#230).

**Phase 3–4 require the headless compositor CI job** but can run locally on a GNOME desktop with `GSK_RENDERER=cairo`.

**Phase 5 is a process change** — every PR that adds or modifies UI behaviour should include integration tests.

## Edge Cases

- **GObject lifecycle in tests** — widgets may be dropped before async callbacks fire. Use `glib::timeout_add_local` with `wait_until()` instead of raw `spawn_local`.
- **GSettings in CI** — CI has no `dconf` database. Use `GSETTINGS_BACKEND=memory` or mock settings.
- **GResources in tests** — UI templates need compiled GResources. Tests that instantiate template-based widgets must call `gio::resources_register()` first, or use a build script that compiles resources for tests.
- **Tokio + GTK in the same test** — use `#[gtk::test]` for the GTK main loop and spawn Tokio tasks via a `tokio::runtime::Handle` created in test setup. Don't use `#[tokio::test]` for tests that need GTK.
- **Test isolation** — each test creates its own `EventBus`, `TempDir`, and mock library. No shared state between tests. This is why `--test-threads=1` is required (GTK global state) but test data is still isolated.
- **Flaky timing** — use `wait_until(condition, timeout_ms)` instead of `sleep()`. Poll the main loop until the expected state is reached or timeout fires.
- **Wayland vs X11 in CI** — `mutter --headless` is the primary approach. If the CI container doesn't have mutter (e.g. minimal images), fall back to `xvfb-run`. Both work identically from the test's perspective — GTK abstracts the display protocol. Set `GDK_BACKEND=wayland` explicitly if needed, but mutter sets this automatically.
- **GNOME 50+ (no X11)** — Xvfb will not be available on distributions that fully remove X11. The `mutter --headless` approach is forward-compatible. Tests should never depend on X11-specific behaviour.
- **SSH / no desktop** — `mutter --headless` works over SSH with no `DISPLAY` or `WAYLAND_DISPLAY` set. Mutter creates its own Wayland socket. Wrap in `dbus-run-session` to provide the session bus that GSettings and portals expect.

# Design: GObject Subclass & Blueprint Refactor (#417)

**Status:** Approved
**Issue:** [#417](https://github.com/justinf555/Moments/issues/417)
**Parent:** [#379](https://github.com/justinf555/Moments/issues/379) (file splits — prerequisite, completed)

---

## Problem

Seven view/model structs are plain Rust structs that pass service dependencies
(`Arc<dyn Library>`, `tokio::runtime::Handle`, `EventSender`) through
constructors. Every signal handler must clone these dependencies into closures,
creating repetitive boilerplate and triggering `clippy::too_many_arguments`.

This is inconsistent with the rest of the codebase — `MomentsWindow`,
`MomentsSidebar`, `PhotoGridCell`, `AlbumCard`, and all setup pages already use
the GObject `mod imp {}` pattern.

## Audit results

A full audit of `src/ui/` found:

- **14 structs** already use the GObject `mod imp {}` pattern
- **7 structs** are plain Rust acting as widgets with service dependencies (conversion candidates)
- **3 structs** are plain widget wrappers with no service deps (`EmptyLibraryView`, `ContentCoordinator`, `InfoPanel`) — too simple to benefit
- **~16 structs** are plain data/model/helper types — remain as plain Rust

## Architecture

```
┌──────────────────────────────────────────┐
│  GObject Widgets (AlbumGridView, etc.)   │  imp struct holds service deps
│  Signal handlers use self.imp()          │  (library, tokio, bus_sender)
├──────────────────────────────────────────┤
│  Config Structs (plain Rust)             │  PhotoGridConfig, ImportOptions
│  Passed as construction props or methods │  Built via builder if complex
├──────────────────────────────────────────┤
│  Domain Types (plain Rust)               │  MediaFilter, SortOrder, Album
└──────────────────────────────────────────┘
```

- **GObject subclasses** for widgets managing lifecycle, signals, and GTK tree
  participation. Service deps live as `OnceCell` fields on the `imp` struct.
- **Config structs** (plain Rust) for values like columns, sort order, thumbnail
  size. Passed via setup methods.
- **Domain types** (plain Rust) for business logic. Already in place.

## Conversion targets

| # | Struct | File | Service deps | Blueprint | Priority |
|---|--------|------|-------------|-----------|----------|
| 1 | `PhotoViewer` | `viewer.rs` | library, tokio, bus_sender | Yes — static headerbar + overlay + split | First (fewest deps, most static) |
| 2 | `VideoViewer` | `video_viewer.rs` | library, tokio, bus_sender | Yes — similar to PhotoViewer | First (same shape) |
| 3 | `EditPanel` | `viewer/edit_panel.rs` | tokio, library, bus_sender | Partial — section skeleton yes, dynamic sliders no | Second |
| 4 | `AlbumGridView` | `album_grid.rs` | library, tokio, texture_cache, bus_sender | Partial — headerbar/empty state yes, grid no | Third |
| 5 | `PhotoGridView` | `photo_grid.rs` | library, tokio, bus_sender, texture_cache | Partial — headerbar/action bar yes, grid no | Third |
| 6 | `CollectionGridView` | `collection_grid.rs` | library | Partial — headerbar/filter buttons yes | Third |
| 7 | `PhotoGridModel` | `photo_grid/model.rs` | library, bus_sender | No — pure logic, no layout | Last |

### What does NOT get converted

- `EmptyLibraryView` — stateless wrapper, no service deps
- `ContentCoordinator` — stack coordinator, no service deps
- `InfoPanel` — stateless metadata display, no service deps
- `EditSession` — mutable edit data container, not a widget
- All cell/card/item types — already GObject subclasses
- All data structs (`TextureCache`, `CellBindings`, etc.) — not widgets

## Conversion pattern

### Before (plain struct)

```rust
pub struct FooView {
    pub widget: gtk::Box,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    bus_sender: EventSender,
}

impl FooView {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        bus_sender: EventSender,
    ) -> Self {
        let widget = gtk::Box::new(/*...*/);

        // Clone deps into every closure
        let lib = library.clone();
        let tok = tokio.clone();
        let bus = bus_sender.clone();
        button.connect_clicked(move |_| {
            let lib = lib.clone();
            let bus = bus.clone();
            tok.spawn(async move {
                let items = lib.list_media(/*...*/).await;
                let _ = bus.send(AppEvent::Loaded { items });
            });
        });

        Self { widget, library, tokio, bus_sender }
    }
}
```

### After (GObject subclass)

```rust
mod imp {
    use std::cell::OnceCell;

    #[derive(Default)]
    pub struct FooView {
        // Service dependencies — set once via setup()
        pub(super) library: OnceCell<Arc<dyn Library>>,
        pub(super) tokio: OnceCell<tokio::runtime::Handle>,
        pub(super) bus_sender: OnceCell<EventSender>,

        // Mutable UI state
        pub(super) model: RefCell<Option<gio::ListStore>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FooView {
        const NAME: &'static str = "MomentsFooView";
        type Type = super::FooView;
        type ParentType = gtk::Widget;  // or adw::Bin, etc.

        fn class_init(klass: &mut Self::Class) {
            // If using Blueprint:
            klass.bind_template();
            // Set layout manager if needed:
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("foo-view");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();  // If using Blueprint
        }
    }

    impl ObjectImpl for FooView {
        fn constructed(&self) {
            self.parent_constructed();
            // Build static widget tree here (or use Blueprint template)
        }

        fn dispose(&self) {
            // Unparent children if this is a gtk::Widget subclass
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for FooView {}
}

glib::wrapper! {
    pub struct FooView(ObjectSubclass<imp::FooView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl FooView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Inject service dependencies after construction.
    pub fn setup(
        &self,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        bus_sender: EventSender,
    ) {
        let imp = self.imp();
        imp.library.set(library).unwrap();
        imp.tokio.set(tokio).unwrap();
        imp.bus_sender.set(bus_sender).unwrap();

        self.setup_signals();
    }

    fn setup_signals(&self) {
        // Signal handlers access deps via self.imp() — no cloning
        let view_weak = self.downgrade();
        button.connect_clicked(move |_| {
            let Some(view) = view_weak.upgrade() else { return };
            let imp = view.imp();
            let lib = imp.library.get().unwrap().clone();
            let bus = imp.bus_sender.get().unwrap().clone();
            imp.tokio.get().unwrap().spawn(async move {
                let items = lib.list_media(/*...*/).await;
                let _ = bus.send(AppEvent::Loaded { items });
            });
        });
    }
}
```

### Field type guide

| Use case | Field type | Example |
|----------|-----------|---------|
| Service dep (set once, never changes) | `OnceCell<T>` | `library: OnceCell<Arc<dyn Library>>` |
| Scalar UI state (Copy types) | `Cell<T>` | `in_selection_mode: Cell<bool>` |
| Complex mutable state | `RefCell<T>` | `model: RefCell<Option<gio::ListStore>>` |
| Optional binding cleanup | `RefCell<Option<T>>` | `bindings: RefCell<Option<CellBindings>>` |
| Blueprint template children | `#[template_child]` | `pub header: TemplateChild<adw::HeaderBar>` |

### Naming conventions

- GObject `NAME`: `"Moments{StructName}"` (e.g. `"MomentsPhotoViewer"`)
- CSS name: `kebab-case` (e.g. `"photo-viewer"`)
- Blueprint resource: `/io/github/justinf555/Moments/{filename}.ui`

## Blueprint adoption

### Decision criteria

- **Use Blueprint** when the widget has a fixed hierarchy of children with
  static properties (alignment, margins, CSS classes, visibility defaults)
- **Keep in Rust** when children are created/destroyed at runtime, loop-generated
  with variable count, or conditionally constructed based on data
- **Mixed is fine** — template for the skeleton, Rust for dynamic parts
  (event controllers, data binding, runtime visibility toggling)

### Per-widget Blueprint decisions

#### Conversion targets (GObject refactor + Blueprint)

| Widget | Blueprint scope | Stays in Rust |
|--------|----------------|---------------|
| PhotoViewer | Headerbar, overlay, split view, star/info/edit buttons | Navigation logic, full-res loading, edit session |
| VideoViewer | Headerbar, overlay, video controls | Playback state, GStreamer setup |
| EditPanel | Section skeleton (AdwPreferencesPage) | Dynamic slider creation, filter grid, preview |
| AlbumGridView | Headerbar with sort button, empty state | Grid view, model, selection mode |
| PhotoGridView | Headerbar with zoom, action bar | Grid view, model, pagination, selection mode |
| CollectionGridView | Headerbar | Grid view, drill-down navigation |
| PhotoGridModel | N/A | Everything (pure logic) |

#### Existing GObject subclasses (Blueprint-only extraction)

These are already GObject subclasses but build their widget tree programmatically
in `constructed()`. Their hierarchies are entirely static — fixed children with
property assignments — making them strong Blueprint candidates. Converting them
is lower priority (no structural change needed) but improves consistency and
readability.

| Widget | Blueprint scope | Stays in Rust |
|--------|----------------|---------------|
| PhotoGridCell | Overlay + picture + placeholder + star btn + checkbox + labels | Hover controller, bind/unbind |
| AlbumCard | Inner box + frame + overlay + mosaic grid (4 fixed pictures) + checkbox + labels | Hover controller, bind/unbind, cover loading |
| CollectionGridCell | Frame + overlay + placeholder + picture + hidden icon + labels | Bind/unbind, thumbnail loading |

## Implementation plan

### Execution order

1. **PhotoViewer** + **VideoViewer** (parallel PRs or sequential — similar shape,
   fewest deps, most static layout, ideal Blueprint candidates)
2. **EditPanel** (depends on viewer pattern being established)
3. **AlbumGridView** + **PhotoGridView** + **CollectionGridView** (grid views
   share patterns — can be parallel once one is done)
4. **PhotoGridModel** (last — pure logic conversion, no Blueprint)

### Per-struct PR checklist

Each conversion is one PR. Every PR must:

- [ ] Create a feature branch
- [ ] Add `mod imp {}` with `ObjectSubclass`, `ObjectImpl`, `WidgetImpl`
- [ ] Move service deps to `OnceCell` fields on imp struct
- [ ] Replace constructor with `new()` + `setup()` pattern
- [ ] Migrate signal handlers to use `self.imp()` access
- [ ] Extract static layout to `.blp` template (if applicable per table above)
- [ ] Add `.blp` file to `src/io.github.justinf555.Moments.gresource.xml`
- [ ] Add translatable strings to `po/POTFILES.in` (if Blueprint has i18n strings)
- [ ] Remove `#[allow(clippy::too_many_arguments)]` if present
- [ ] Verify: `make lint`, `make test`, `make test-integration`
- [ ] Verify: `make run-dev` — manual smoke test of affected view

### What changes for callers

Callers that currently do:

```rust
let view = PhotoGridView::new(library, tokio, bus_sender, texture_cache);
stack.add_child(&view.widget);
```

Will change to:

```rust
let view = PhotoGridView::new();
view.setup(library, tokio, bus_sender, texture_cache);
stack.add_child(&view);  // GObject IS the widget — no .widget field
```

## PhotoViewer conversion detail

First conversion — establishes the pattern for all subsequent PRs.

### Key decisions

1. **GObject parent type: `adw::NavigationPage`** — the viewer logically *is* a
   navigation page. Eliminates the `.nav_page()` accessor; callers push the
   viewer directly onto the NavigationView.

2. **`Rc<ViewerInner>` → `mod imp {}`** — all fields move to the imp struct.
   The `Rc` shared-ownership pattern is replaced by GObject ref-counting.
   Signal handlers use `self.downgrade()` → `upgrade()` → `imp()` instead of
   `Rc::downgrade(&inner)` → `upgrade()`.

3. **Methods on `impl PhotoViewer` (Option A)** — all methods (including
   `loading.rs` and `menu.rs`) live on the public type, accessing fields via
   `self.imp()`. Consistent with `MomentsWindow`. The `impl ViewerInner` blocks
   in `loading.rs` and `menu.rs` become `impl PhotoViewer`.

4. **Blueprint template** covers the full static widget tree:
   ```
   NavigationPage (tag="viewer")
     └── ToolbarView
           ├── HeaderBar [top bar]
           │     └── [end]: ★ star, ℹ info_toggle, ✏ edit_toggle, ⋮ menu_btn
           └── OverlaySplitView
                 ├── [content] Overlay
                 │     ├── ScrolledWindow → Picture
                 │     ├── Spinner
                 │     ├── Button prev (osd circular)
                 │     └── Button next (osd circular)
                 └── [sidebar] Stack (transition: crossfade)
   ```
   The sidebar stack is declared empty in Blueprint. InfoPanel and EditPanel are
   added programmatically in `setup()` via `stack.add_named()`. When those panels
   become GObject subclasses in later PRs, they'll be referenced directly in the
   template using `$MomentsInfoPanel` / `$MomentsEditPanel` syntax.

5. **Overflow menu** stays in Rust — `build_viewer_menu_popover()` is shared
   with VideoViewer and has conditional content (`include_wallpaper`). It's
   created in `setup()` and attached to the `menu_btn` template child.

### imp struct fields

```rust
#[derive(Default, gtk::CompositeTemplate)]
#[template(resource = "/io/github/justinf555/Moments/viewer.ui")]
pub struct PhotoViewer {
    // Template children (from Blueprint)
    #[template_child] pub(super) toolbar_view: TemplateChild<adw::ToolbarView>,
    #[template_child] pub(super) picture: TemplateChild<gtk::Picture>,
    #[template_child] pub(super) spinner: TemplateChild<gtk::Spinner>,
    #[template_child] pub(super) prev_btn: TemplateChild<gtk::Button>,
    #[template_child] pub(super) next_btn: TemplateChild<gtk::Button>,
    #[template_child] pub(super) star_btn: TemplateChild<gtk::Button>,
    #[template_child] pub(super) info_split: TemplateChild<adw::OverlaySplitView>,
    #[template_child] pub(super) sidebar_stack: TemplateChild<gtk::Stack>,
    #[template_child] pub(super) info_toggle: TemplateChild<gtk::ToggleButton>,
    #[template_child] pub(super) edit_toggle: TemplateChild<gtk::ToggleButton>,
    #[template_child] pub(super) menu_btn: TemplateChild<gtk::MenuButton>,

    // Service dependencies (set once in setup)
    pub(super) library: OnceCell<Arc<dyn Library>>,
    pub(super) tokio: OnceCell<tokio::runtime::Handle>,
    pub(super) bus_sender: OnceCell<EventSender>,

    // Owned sub-panels (set in setup, not GObject yet)
    pub(super) info_panel: RefCell<Option<InfoPanel>>,
    pub(super) edit_panel: RefCell<Option<EditPanel>>,

    // Mutable state
    pub(super) items: RefCell<Vec<MediaItemObject>>,
    pub(super) current_index: Cell<usize>,
    pub(super) load_gen: Cell<u64>,
    pub(super) pending_load: RefCell<Option<MediaId>>,
    pub(super) current_metadata: RefCell<Option<MediaMetadataRecord>>,
    pub(super) pending_fav: RefCell<Option<(MediaId, bool)>>,
}
```

### Caller changes

Before:
```rust
let viewer = PhotoViewer::new(library, tokio, bus_sender);
nav_view.push(viewer.nav_page());
viewer.show(items, index);
```

After:
```rust
let viewer = PhotoViewer::new();
viewer.setup(library, tokio, bus_sender);
nav_view.push(&viewer);  // viewer IS the NavigationPage
viewer.show(items, index);
```

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| Blueprint compilation errors caught late | `make check` includes Blueprint compilation via gresource |
| Signal handler regressions | Each PR includes manual smoke test of affected view |
| Inconsistent patterns across 7 PRs | This doc serves as the reference; first PR (PhotoViewer) establishes the template |
| `OnceCell` panics if `setup()` not called | Debug assert in first method that accesses deps; caught by existing tests |

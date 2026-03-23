# Design: Lazy View Loading

**Issue:** [#64](https://github.com/justinf555/Moments/issues/64)
**Status:** Proposed
**Date:** 2026-03-24

## Prior Art: GNOME App Patterns

Research into how other GNOME apps handle sidebar navigation and view lifecycle:

| App | View Creation | View Lifecycle | Approach |
|-----|--------------|----------------|----------|
| **Fractal** | Eager — fixed set in GtkStack | Singleton views reconfigured with new data on selection change | Small fixed set of view types, reused across room selections |
| **Nautilus** | Lazy — per slot/tab | Single view created on first navigation, reused within slot | Views persist with their own back/forward history |
| **GNOME Settings** | Lazy — on click | Created fresh each time, destroyed on switch | No caching, simplest pattern, minimal memory |

### Why We Chose the Nautilus Pattern

Our design aligns closest with **Nautilus** — lazy creation on first navigation, then the view persists and maintains its own state:

- **Fractal's pattern** (eager singletons reconfigured with new data) is what we had before the separate-instances refactor. We moved away because reconfiguring one shared view lost scroll position, required pop_to_grid hacks, and leaked state between routes.
- **GNOME Settings' pattern** (create/destroy on every switch) would work but wastes the DB query on every visit and loses scroll position, zoom level, and viewer state.
- **Nautilus' pattern** (lazy creation, persistent views) fits our needs: views are moderately expensive to create (model + DB query + GridView + factory), and once created they maintain valuable state (scroll position, zoom, viewer page).

### Moments-Specific: ModelRegistry

Our `ModelRegistry` pattern for event broadcasting is specific to Moments. It doesn't appear in Fractal (single shared data model), Nautilus (no equivalent event bus), or GNOME Settings (no persistent models). It's needed because our architecture has multiple independent models that must receive the same library events (`ThumbnailReady`, `ImportComplete`).

## Problem

All sidebar views are eagerly constructed at startup in `window.rs::setup()`. Each `PhotoGridView` creates a `PhotoGridModel` that immediately calls `load_more()`, firing a database query. With N sidebar routes, that's N queries at startup even if the user only ever looks at Photos.

This doesn't scale as more views are added (Recent Imports, Albums, Trash, Tags). Each additional route adds startup latency and memory overhead for views the user may never visit.

## Current Architecture

```
window.setup()
├── creates PhotoGridModel (filter=All)         ← DB query fires immediately
├── creates PhotoGridView ("photos")
├── creates PhotoGridModel (filter=Favorites)   ← DB query fires immediately
├── creates PhotoGridView ("favorites")
├── registers both in ContentCoordinator
└── returns Vec<Rc<PhotoGridModel>> to application.rs
        └── idle loop iterates this vec for ThumbnailReady / ImportComplete
```

The `Vec<Rc<PhotoGridModel>>` is captured by the idle loop closure at startup. It's a fixed-size list — models created later can't join it.

## Proposed Architecture

```
window.setup()
├── creates ModelRegistry (shared, growable)
├── creates PhotoGridModel (filter=All)         ← DB query fires immediately
├── creates PhotoGridView ("photos")
├── registers model in ModelRegistry
├── registers view eagerly in ContentCoordinator
├── registers "favorites" LAZILY in ContentCoordinator (factory closure)
└── returns Rc<ModelRegistry> to application.rs
        └── idle loop calls registry.on_thumbnail_ready() / registry.reload_all()

First click on "Favorites":
├── ContentCoordinator::navigate("favorites")
├── ViewSlot::Lazy → calls factory closure
│   ├── creates PhotoGridModel (filter=Favorites)   ← DB query fires NOW
│   ├── creates PhotoGridView
│   ├── registers model in ModelRegistry
│   └── returns Rc<dyn ContentView>
├── adds widget to GtkStack
├── replaces ViewSlot::Lazy with ViewSlot::Ready
└── switches stack to "favorites"
```

## New Type: ModelRegistry

### Purpose

A shared, growable list of `PhotoGridModel` instances. Solves the problem of the idle loop needing to broadcast events to models that don't exist yet at startup.

### Location

`src/ui/model_registry.rs`

### Interface

```rust
use std::cell::RefCell;
use std::rc::Rc;

use crate::library::media::MediaId;
use crate::ui::photo_grid::PhotoGridModel;

/// Shared registry of all active PhotoGridModel instances.
///
/// The application's idle loop calls `on_thumbnail_ready` and `reload_all`
/// to broadcast library events. Models register themselves at creation
/// time — either during startup (eager) or on first navigation (lazy).
pub struct ModelRegistry {
    models: RefCell<Vec<Rc<PhotoGridModel>>>,
}

impl ModelRegistry {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            models: RefCell::new(Vec::new()),
        })
    }

    /// Add a model to the registry. Called when a view is created.
    pub fn register(&self, model: &Rc<PhotoGridModel>) {
        self.models.borrow_mut().push(Rc::clone(model));
    }

    /// Forward a ThumbnailReady event to all registered models.
    pub fn on_thumbnail_ready(&self, id: &MediaId) {
        for model in self.models.borrow().iter() {
            model.on_thumbnail_ready(id);
        }
    }

    /// Reload all registered models (e.g. after import completes).
    pub fn reload_all(&self) {
        for model in self.models.borrow().iter() {
            model.reload();
        }
    }
}
```

### Why not just `Rc<RefCell<Vec<...>>>`?

A dedicated type:
- Gives the concept a name (`ModelRegistry` is clearer than a nested generic)
- Encapsulates the broadcast logic (callers don't iterate manually)
- Can be extended later (e.g., unregister on view disposal, filter-aware broadcasting)

## Changes to ContentCoordinator

### ViewSlot Enum

```rust
enum ViewSlot {
    /// View is constructed and its widget is in the stack.
    Ready(Rc<dyn ContentView>),
    /// View will be constructed on first navigate().
    /// The factory closure creates the view and registers its model.
    Lazy(Option<Box<dyn FnOnce() -> Rc<dyn ContentView>>>),
}
```

The `Option` wrapper on `Lazy` allows `take()` to move the `FnOnce` out of the slot. After materialisation, the slot is replaced with `Ready`.

### New Method: register_lazy

```rust
pub fn register_lazy<F>(&mut self, id: &str, factory: F)
where
    F: FnOnce() -> Rc<dyn ContentView> + 'static,
{
    self.slots.insert(id.to_owned(), ViewSlot::Lazy(Some(Box::new(factory))));
}
```

No widget is added to the stack yet — that happens on first `navigate()`.

### Updated navigate()

```rust
pub fn navigate(&mut self, id: &str) {
    let Some(slot) = self.slots.get_mut(id) else {
        warn!(route = %id, "navigate: unknown route");
        return;
    };

    // Materialise lazy views on first access.
    if let ViewSlot::Lazy(factory) = slot {
        let factory = factory.take().expect("lazy factory called once");
        let view = factory();
        self.stack.add_named(view.widget(), Some(id));
        *slot = ViewSlot::Ready(view);
    }

    self.stack.set_visible_child_name(id);
}
```

**Note:** `navigate` now requires `&mut self`. Since the coordinator is behind `Rc<RefCell<>>`, callers use `coordinator.borrow_mut().navigate(id)`.

### Full Updated Coordinator

```rust
use std::collections::HashMap;
use std::rc::Rc;

use tracing::warn;

use super::ContentView;

enum ViewSlot {
    Ready(Rc<dyn ContentView>),
    Lazy(Option<Box<dyn FnOnce() -> Rc<dyn ContentView>>>),
}

pub struct ContentCoordinator {
    stack: gtk::Stack,
    slots: HashMap<String, ViewSlot>,
}

impl ContentCoordinator {
    pub fn new(stack: gtk::Stack) -> Self {
        Self {
            stack,
            slots: HashMap::new(),
        }
    }

    pub fn register(&mut self, id: &str, view: Rc<dyn ContentView>) {
        self.stack.add_named(view.widget(), Some(id));
        self.slots.insert(id.to_owned(), ViewSlot::Ready(view));
    }

    pub fn register_lazy<F>(&mut self, id: &str, factory: F)
    where
        F: FnOnce() -> Rc<dyn ContentView> + 'static,
    {
        self.slots.insert(id.to_owned(), ViewSlot::Lazy(Some(Box::new(factory))));
    }

    pub fn navigate(&mut self, id: &str) {
        let Some(slot) = self.slots.get_mut(id) else {
            warn!(route = %id, "navigate: unknown route");
            return;
        };

        if let ViewSlot::Lazy(factory) = slot {
            let factory = factory.take().expect("lazy factory called once");
            let view = factory();
            self.stack.add_named(view.widget(), Some(id));
            *slot = ViewSlot::Ready(view);
        }

        self.stack.set_visible_child_name(id);
    }
}
```

## Changes to window.rs

### setup() Return Type

Before: `Vec<Rc<PhotoGridModel>>`
After: `Rc<ModelRegistry>`

### View Registration

```rust
use crate::ui::model_registry::ModelRegistry;

pub fn setup(&self, library: Arc<dyn Library>, tokio: Handle, settings: Settings)
    -> Rc<ModelRegistry>
{
    let registry = ModelRegistry::new();

    // Photos: eager (always the default view)
    let photos_model = Rc::new(PhotoGridModel::new(
        Arc::clone(&library), tokio.clone(), MediaFilter::All,
    ));
    let photos_view = Rc::new(PhotoGridView::new(
        Arc::clone(&library), tokio.clone(), settings.clone(),
    ));
    photos_view.set_model(Rc::clone(&photos_model));
    registry.register(&photos_model);
    self.insert_action_group("view", Some(photos_view.view_actions()));
    coordinator.register("photos", photos_view);

    // Favorites: lazy — created on first navigation
    {
        let lib = Arc::clone(&library);
        let tk = tokio.clone();
        let s = settings.clone();
        let reg = Rc::clone(&registry);
        coordinator.register_lazy("favorites", move || {
            let model = Rc::new(PhotoGridModel::new(lib, tk, MediaFilter::Favorites));
            let view = Rc::new(PhotoGridView::new(/* ... */));
            view.set_model(Rc::clone(&model));
            reg.register(&model);
            view as Rc<dyn ContentView>
        });
    }

    // ... rest of setup ...

    registry
}
```

## Changes to application.rs

### Idle Loop

Before:
```rust
let models = window.setup(library, tokio.clone(), settings);
// ...
Ok(LibraryEvent::ThumbnailReady { media_id }) => {
    for m in &models {
        m.on_thumbnail_ready(&media_id);
    }
}
Ok(LibraryEvent::ImportComplete(summary)) => {
    // ...
    for m in &models {
        m.reload();
    }
}
```

After:
```rust
let registry = window.setup(library, tokio.clone(), settings);
// ...
Ok(LibraryEvent::ThumbnailReady { media_id }) => {
    registry.on_thumbnail_ready(&media_id);
}
Ok(LibraryEvent::ImportComplete(summary)) => {
    // ...
    registry.reload_all();
}
```

### Shutdown

Store the registry on the application so it's dropped during `shutdown()`:

```rust
pub struct MomentsApplication {
    // ...
    pub model_registry: RefCell<Option<Rc<ModelRegistry>>>,
}
```

In `shutdown()`:
```rust
self.model_registry.borrow_mut().take();
```

This releases all `Rc<PhotoGridModel>` references, which in turn release `Arc<dyn Library>` → `SqlitePool` can shut down cleanly before Tokio drops.

## Behaviour Matrix

| Route | When view is created | When DB query fires | Events received |
|-------|---------------------|-------------------|-----------------|
| Photos | Startup | Startup | From startup |
| Favorites | First click | First click | From first click onward |
| Future: Recent | First click | First click | From first click onward |
| Future: Albums | First click | First click | From first click onward |
| Empty | Startup | Never (no model) | N/A |

## Edge Cases

### ThumbnailReady before Favorites is materialised

No problem — the event is broadcast to all registered models. If Favorites hasn't been created yet, its model isn't registered, so it doesn't receive the event. When Favorites is materialised, `load_more()` fetches current data from the DB (which already has the thumbnail state).

### ImportComplete before Favorites is materialised

Same logic — `reload_all()` only iterates registered models. When Favorites is eventually created, it loads fresh data that includes the imported items.

### User never clicks Favorites

The factory closure is never called. No model, no view, no DB query, no memory allocation. The closure itself is dropped when the coordinator is dropped during shutdown.

## Files Changed Summary

| File | Change |
|------|--------|
| `src/ui/model_registry.rs` | **New** — shared model list with event broadcast |
| `src/ui.rs` | Add `pub mod model_registry` |
| `src/ui/coordinator.rs` | `ViewSlot` enum, `register_lazy()`, `navigate()` takes `&mut self` |
| `src/ui/window.rs` | Return `Rc<ModelRegistry>`, lazy-register Favorites |
| `src/application.rs` | Use `ModelRegistry` in idle loop and shutdown |

## What Stays the Same

- `PhotoGridModel` — no changes
- `PhotoGridView` — no changes
- `PhotoGrid`, cells, factory, viewer — no changes
- `ContentView` trait — no changes
- Sidebar routes — no changes
- Event channel — no changes
- GSettings — no changes

## Testing

- Existing 69 tests continue to pass (no model/view logic changes)
- Manual test: Favorites view should not query DB until first click
- Manual test: starring a photo in Photos, then clicking Favorites, should show it (DB is source of truth)
- Manual test: importing photos should update both views (if Favorites was already visited)

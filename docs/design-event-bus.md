# Design: Centralised Event Bus (#230)

**Status:** Proposed (revised after external review)
**Issue:** [#230](https://github.com/justinf555/Moments/issues/230)

---

## Problem

The current architecture routes all library events through a single `std::sync::mpsc` channel consumed by an idle loop in `application.rs`. This loop manually dispatches every event variant to the appropriate models and UI components. As the app grows, this creates two problems:

### 1. God dispatcher

The idle loop in `application.rs` (lines 489–616) knows about every model, every sidebar method, every dialog, and every event type. Adding a new event or subscriber means modifying this centralised switch statement.

### 2. Clone chains

UI action handlers (buttons, context menus, action bars) need `library`, `tokio`, `registry`, and various widget references to perform async work and broadcast results. These are cloned through multiple closure layers:

```
Button clicked
  → clone library, tokio, registry, exit_selection
    → spawn_local
      → clone library, tokio, registry again
        → tokio.spawn
          → call library method
        → on success: registry.on_trashed()
        → exit_selection.activate()
```

A single "trash selected items" action requires **12+ clones** across 3 nested closure layers. The `ActionBarFactory` passes `exit_selection` through 4 function signatures just so the trash handler can exit selection mode after completion.

---

## Current Architecture

```
┌──────────────────────────────────────────────────────┐
│                    Library Backend                     │
│  ImportJob · SyncManager · Thumbnailer                │
│                      │                                │
│              Sender<LibraryEvent>                     │
└──────────────────────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│              application.rs idle loop                  │
│                                                       │
│  match event {                                        │
│    ThumbnailReady => registry.on_thumbnail_ready()    │
│    ImportProgress => sidebar.show_upload_progress()   │
│    ImportComplete => registry.reload_all()            │
│    AssetSynced    => registry.on_asset_synced()       │
│    SyncStarted    => sidebar.show_sync_started()      │
│    SyncComplete   => sidebar.show_sync_complete()     │
│    AlbumCreated   => sidebar.add_album()              │
│    ... (18 variants, each hand-routed)                │
│  }                                                    │
└──────────────────────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│                  ModelRegistry                         │
│                                                       │
│  Vec<Rc<PhotoGridModel>>                              │
│  on_thumbnail_ready() → all models                    │
│  on_favorite_changed() → all models                   │
│  on_trashed() → all models                            │
│  on_deleted() → all models                            │
│  on_asset_synced() → all models                       │
│  reload_all() → all models                            │
└──────────────────────────────────────────────────────┘
```

**Event producers:** `sync.rs`, `importer.rs`, `immich_importer.rs`, `thumbnailer.rs`, `local.rs`, `immich.rs` — all send `LibraryEvent` via the shared `Sender`.

**Event consumers:** `application.rs` (the only consumer) routes to `ModelRegistry`, `Sidebar`, `ImportDialog`, and `Window`.

**UI actions:** Button handlers in `action_bar.rs`, `actions.rs`, and `photo_grid.rs` call library methods directly, then broadcast results via `ModelRegistry`. They don't use the event channel at all — they clone `library` + `registry` into closures.

---

## Proposed Architecture

### Channel primitive: `glib::MainContext::channel`

Use GLib's native async channel — **not** `tokio::sync::broadcast`. GLib channels deliver events directly into the main loop via its native dispatch mechanism. No polling timers, no wasted CPU when idle, no lagged receivers.

```rust
let (tx, rx) = glib::MainContext::channel::<AppEvent>(glib::Priority::DEFAULT);

// Background sender (any thread):
tx.send(AppEvent::ThumbnailReady { media_id }).unwrap();

// GTK main thread — push-based, zero latency:
rx.attach(None, move |event| {
    // handle event
    glib::ControlFlow::Continue
});
```

Key properties:
- **Push, not poll** — events delivered via the main loop's native dispatch, not a 16ms timer
- **Unbounded** — correctness-critical events (trash, delete, favourite) are never dropped
- **Thread-safe sender** — `glib::Sender` is `Send`, can be used from Tokio tasks
- **Single receiver** — each channel has one consumer (see multi-subscriber pattern below)

### Multi-subscriber pattern

`glib::MainContext::channel` has one receiver per channel. To support multiple subscribers, we use a **fan-out dispatcher**: one channel receives all events, one dispatcher callback routes to registered subscribers.

```rust
pub struct EventBus {
    tx: glib::Sender<AppEvent>,
    subscribers: Rc<RefCell<Vec<Box<dyn Fn(&AppEvent)>>>>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, rx) = glib::MainContext::channel::<AppEvent>(glib::Priority::DEFAULT);
        let subscribers: Rc<RefCell<Vec<Box<dyn Fn(&AppEvent)>>>> =
            Rc::new(RefCell::new(Vec::new()));

        let subs = Rc::clone(&subscribers);
        rx.attach(None, move |event| {
            for handler in subs.borrow().iter() {
                handler(&event);
            }
            glib::ControlFlow::Continue
        });

        Self { tx, subscribers }
    }

    /// Get a sender for producing events (thread-safe, cloneable).
    pub fn sender(&self) -> glib::Sender<AppEvent> {
        self.tx.clone()
    }

    /// Register a subscriber callback. Called on the GTK main thread.
    pub fn subscribe(&self, handler: impl Fn(&AppEvent) + 'static) {
        self.subscribers.borrow_mut().push(Box::new(handler));
    }
}
```

Each component calls `bus.subscribe(...)` with a closure that handles the events it cares about. The fan-out happens in one place — no per-component timers.

### Layer boundary: LibraryEvent stays in the library

The library layer continues to send `LibraryEvent` via `std::sync::mpsc` (or `glib::Sender<LibraryEvent>` if we migrate the channel type). A thin **event translator** at the application boundary converts `LibraryEvent` → `AppEvent` and forwards to the bus:

```
┌──────────────────────────────────────────────────────┐
│                    Library Backend                     │
│  ImportJob · SyncManager · Thumbnailer                │
│                      │                                │
│          Sender<LibraryEvent>  (library-layer type)   │
└──────────────────────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│              Event Translator (application.rs)        │
│                                                       │
│  LibraryEvent::ThumbnailReady → AppEvent::ThumbnailReady
│  LibraryEvent::SyncStarted   → AppEvent::SyncStarted │
│  (thin mapping, no routing logic)                     │
└──────────────────────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│                    EventBus                            │
│              glib::MainContext::channel                │
│                      │                                │
│              fan-out to subscribers                    │
└──────────────────────────────────────────────────────┘
              │        │        │        │
              ▼        ▼        ▼        ▼
          PhotoGrid  Sidebar  Command   Selection
          Model      Status   Dispatch  Controller
```

This preserves the dependency hierarchy: **library knows nothing about `AppEvent`**. The translator is a simple match that maps variants 1:1. It replaces the god dispatcher's routing logic with pure translation — no references to models, sidebar, or dialogs.

```rust
// In application.rs — replaces the idle loop.
// The library sends LibraryEvent via glib::Sender<LibraryEvent> (migrated
// from std::sync::mpsc), so delivery is push-based all the way through —
// no polling timer needed.
fn start_event_translator(
    library_rx: glib::Receiver<LibraryEvent>,
    bus: &EventBus,
) {
    let tx = bus.sender();
    // Push-based: rx.attach runs the callback whenever an event arrives,
    // driven by the GLib main loop — no polling, no wasted cycles at idle.
    library_rx.attach(None, move |event| {
        let app_event = match event {
            LibraryEvent::ThumbnailReady { media_id } => AppEvent::ThumbnailReady { media_id },
            LibraryEvent::SyncStarted => AppEvent::SyncStarted,
            LibraryEvent::ImportComplete(summary) => AppEvent::ImportComplete { summary },
            // ... 1:1 mapping, no routing logic
            _ => return glib::ControlFlow::Continue,
        };
        let _ = tx.send(app_event);
        glib::ControlFlow::Continue
    });
}
```

### AppEvent enum

```rust
pub enum AppEvent {
    // ── Lifecycle ────────────────────────────────────
    Ready,
    ShutdownComplete,
    Error(String),

    // ── Import ───────────────────────────────────────
    ImportProgress { current: usize, total: usize, imported: usize, skipped: usize, failed: usize },
    ImportComplete { summary: ImportSummary },

    // ── Thumbnails ───────────────────────────────────
    ThumbnailReady { media_id: MediaId },
    ThumbnailDownloadProgress { completed: usize, total: usize },
    ThumbnailDownloadsComplete { total: usize },

    // ── Commands (UI intent → CommandDispatcher) ─────
    TrashRequested { ids: Vec<MediaId> },
    RestoreRequested { ids: Vec<MediaId> },
    DeleteRequested { ids: Vec<MediaId> },
    FavoriteRequested { ids: Vec<MediaId>, state: bool },
    RemoveFromAlbumRequested { album_id: AlbumId, ids: Vec<MediaId> },

    // ── Results (CommandDispatcher → subscribers) ────
    FavoriteChanged { ids: Vec<MediaId>, is_favorite: bool },
    Trashed { ids: Vec<MediaId> },
    Restored { ids: Vec<MediaId> },
    Deleted { ids: Vec<MediaId> },
    AssetSynced { item: MediaItem },
    AssetDeletedRemote { media_id: MediaId },

    // ── Albums ───────────────────────────────────────
    AlbumCreated { id: AlbumId, name: String },
    AlbumRenamed { id: AlbumId, name: String },
    AlbumDeleted { id: AlbumId },
    AlbumMediaChanged { album_id: AlbumId },

    // ── Sync ─────────────────────────────────────────
    SyncStarted,
    SyncProgress { assets: usize, people: usize, faces: usize },
    SyncComplete { assets: usize, people: usize, faces: usize, errors: usize },
    PeopleSyncComplete,
}
```

Key design decisions:
- **No `ExitSelectionMode`** — selection mode is pure view state. Use the existing `view.exit-selection` GAction directly. Components that need to exit selection mode after an action (e.g. after `Trashed`) call the GAction in their subscriber, not via the bus.
- **`LibraryEvent` stays** as the library-layer type. `AppEvent` is application-layer only.
- **`AppEvent` must be `Clone`** — the `CommandDispatcher` clones the event before spawning a Tokio task (`let evt = event.clone()`). `MediaItem`, `MediaId`, `AlbumId`, and `ImportSummary` all need `#[derive(Clone)]`.
- **Library channel migrated to `glib::Sender`** — the library sends `LibraryEvent` via `glib::Sender` (not `std::sync::mpsc`), so the translator uses push-based `rx.attach()` with no polling timer. Push delivery all the way through.

---

### Design principle: self-contained components

**Event handlers must live inside the component that owns the behaviour, never in a parent.**

Parent components (`window.rs`, `application.rs`) are responsible for **assembly only** — creating child components and placing them in the layout. They must never route events to children or wire callbacks between siblings. Each component subscribes to the bus in its own constructor and handles its own events internally.

This ensures separation of concerns: adding a new event or changing how a component reacts to an event requires modifying only that component's file, not the parent that assembled it.

```rust
// ✅ CORRECT — component subscribes internally
let sidebar = MomentsSidebar::new(&bus);
// Done. Parent has no knowledge of what events sidebar handles.

// ❌ WRONG — parent routes events to child
let sidebar = MomentsSidebar::new();
// ...later in an idle loop or callback in window.rs:
match event {
    SyncStarted => sidebar.show_sync_started(),  // parent knows too much
}
```

Every component constructor takes `bus: &EventBus` and calls `bus.subscribe(...)` internally. The subscriber closure captures a weak reference to the component — when the component is dropped, the callback becomes a no-op.

```rust
impl MomentsSidebar {
    pub fn new(bus: &EventBus) -> Self {
        let sidebar = Self { /* build UI */ };

        let weak = sidebar.downgrade();
        bus.subscribe(move |event| {
            let Some(s) = weak.upgrade() else { return };
            match event {
                AppEvent::SyncStarted => s.show_sync_started(),
                AppEvent::SyncProgress { .. } => s.show_sync_progress(..),
                AppEvent::AlbumCreated { id, name } => s.add_album(id, name),
                AppEvent::ImportProgress { .. } => s.show_upload_progress(..),
                _ => {}
            }
        });

        sidebar
    }
}
```

This pattern applies to every component that reacts to events:

| Component | Constructor | Events handled internally |
|-----------|------------|--------------------------|
| `MomentsSidebar::new(bus)` | Sync, import, album events | Yes |
| `PhotoGridModel::new(lib, tk, filter, bus)` | Thumbnail, media state events | Yes |
| `PhotoGridView::new(lib, tk, bus)` | `Trashed`/`Deleted` → activates `view.exit-selection` GAction | Yes |
| `ImportDialog::new(bus)` | Import progress/complete | Yes |

`window.rs` becomes pure assembly — create components, place in layout, done.

---

### Command / result event pattern

Events are split into two categories:

- **Command events** (`*Requested`) — UI intent. Emitted by buttons. Carry the minimum data the UI can resolve (e.g. selected IDs).
- **Result events** (`*Changed`, `*Completed`) — outcomes. Emitted by the command dispatcher after the library operation succeeds. Consumed by models, sidebar, selection controller.

This separates concerns cleanly: **UI resolves UI state → command dispatcher does library work → result event drives all downstream effects.**

#### Command dispatcher: trait-based dispatch

Each command is its own struct implementing the `CommandHandler` trait. A `CommandDispatcher` owns `library` and `tokio`, subscribes to the bus, and routes each event to the handler that claims it.

This follows the Strategy/Command pattern: adding a new command means creating one struct in one file and registering one line. Zero modification to existing commands.

```rust
// src/commands/mod.rs

/// Trait for a single command handler.
#[async_trait]
pub trait CommandHandler: Send + Sync {
    /// Returns true if this handler can process the given event.
    fn handles(&self, event: &AppEvent) -> bool;

    /// Execute the command. Called on the Tokio runtime.
    /// On success, sends the result event via the bus sender.
    /// On failure, sends AppEvent::Error with a user-facing message.
    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &glib::Sender<AppEvent>,
    );
}
```

Each command is a small, single-responsibility struct:

```rust
// src/commands/trash.rs
pub struct TrashCommand;

#[async_trait]
impl CommandHandler for TrashCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::TrashRequested { .. })
    }

    async fn execute(&self, event: AppEvent, library: &Arc<dyn Library>, bus: &glib::Sender<AppEvent>) {
        let AppEvent::TrashRequested { ids } = event else { return };
        match library.trash(&ids).await {
            Ok(()) => { let _ = bus.send(AppEvent::Trashed { ids }); }
            Err(e) => { let _ = bus.send(AppEvent::Error(format!("Failed to move to trash: {e}"))); }
        }
    }
}
```

The dispatcher subscribes to the bus and routes commands to handlers. Each command is spawned as an independent Tokio task — this is concurrent, not sequential. The burst rate for commands is user-driven (button clicks), so unbounded spawning is acceptable in practice. If burst concerns arise (e.g. batch operations), a `tokio::sync::mpsc` queue drained by a single worker can be added later.

```rust
// src/commands/dispatcher.rs
pub struct CommandDispatcher;

impl CommandDispatcher {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        bus: &EventBus,
    ) -> Self {
        let handlers: Vec<Box<dyn CommandHandler>> = vec![
            Box::new(TrashCommand),
            Box::new(RestoreCommand),
            Box::new(DeleteCommand),
            Box::new(FavoriteCommand),
            Box::new(AddToAlbumCommand),
            Box::new(RemoveFromAlbumCommand),
            // Adding a new command = one line here + one file.
        ];

        let tx = bus.sender();

        bus.subscribe(move |event| {
            for handler in &handlers {
                if handler.handles(event) {
                    let lib = Arc::clone(&library);
                    let bus_tx = tx.clone();
                    let evt = event.clone();
                    tokio.spawn(async move {
                        handler.execute(evt, &lib, &bus_tx).await;
                    });
                    break;
                }
            }
        });

        Self
    }
}
```

**Scaling:** Adding sharing support means creating `ShareCommand`, `CreateSharedAlbumCommand`, etc. — one file each, one registration line, zero changes to existing commands.

```
src/commands/
  mod.rs              — CommandHandler trait + CommandDispatcher
  trash.rs            — TrashCommand
  restore.rs          — RestoreCommand
  delete.rs           — DeleteCommand
  favorite.rs         — FavoriteCommand
  add_to_album.rs     — AddToAlbumCommand
  remove_from_album.rs — RemoveFromAlbumCommand
  share.rs            — ShareCommand (future)
  shared_album.rs     — CreateSharedAlbumCommand (future)
```

`library` and `tokio` exist in exactly **one place** — the dispatcher. No other component needs them for action execution.

#### Error handling

Every command handler must handle failure explicitly. On error, the handler sends `AppEvent::Error(message)` with a user-facing message. The `Application` component subscribes and shows a toast:

```rust
// In application.rs subscriber:
AppEvent::Error(msg) => {
    if let Some(win) = app.active_window() {
        win.activate_action("win.show-toast", Some(&msg.to_variant()));
    }
}
```

No command failure is ever silently swallowed.

#### Button handlers become trivial

Buttons resolve UI state (selection → IDs) and emit a command. Nothing else.

```rust
// Before (12+ clones, 3 closure layers, 25 lines):
fn wire_trash(btn, selection, library, tokio, registry, exit_selection) { ... }

// After (2 clones, 1 closure layer, 6 lines):
fn wire_trash(btn: &gtk::Button, selection: &gtk::MultiSelection, bus: &EventBus) {
    let sel = selection.clone();
    let tx = bus.sender();
    btn.connect_clicked(move |_| {
        let ids = collect_selected_ids(&sel);
        if !ids.is_empty() {
            let _ = tx.send(AppEvent::TrashRequested { ids });
        }
    });
}
```

#### ActionBarFactory simplified

The factory needs only `selection` (UI state) and `bus` (communication). No `library`, no `tokio`, no `registry`, no `exit_selection`:

```rust
pub fn build_for_filter(
    filter: &MediaFilter,
    selection: &gtk::MultiSelection,
    bus: &EventBus,
) -> ActionBarButtons {
    match filter {
        MediaFilter::Trashed => build_trash_bar(selection, bus),
        MediaFilter::Album { album_id } => build_album_bar(selection, bus, album_id),
        _ => build_standard_bar(selection, bus),
    }
}
```

#### Full event flow for "trash 3 selected items"

```
User clicks "Delete" button
│
├─ Button handler (action_bar.rs):
│   ids = collect_selected_ids(selection)        // [id_1, id_2, id_3]
│   tx.send(TrashRequested { ids })              // done, 2 lines
│
├─ CommandDispatcher receives TrashRequested:
│   TrashCommand.execute() on Tokio:
│     library.trash(&ids).await                  // async DB call
│     tx.send(Trashed { ids })                   // emit result
│     (or on failure: tx.send(Error("..."))      // emit error
│
├─ PhotoGridModel (Photos) receives Trashed:
│   for id in ids { self.remove_item(id) }       // items disappear
│
├─ PhotoGridModel (Trash) receives Trashed:
│   for id in ids { self.fetch_and_insert(id) }  // items appear in trash
│
├─ PhotoGridView receives Trashed:
│   view.exit-selection GAction activated         // exits selection mode
│
├─ Application receives Error (if failed):
│   shows toast via win.show-toast GAction
│
└─ Sidebar receives Trashed:
    (could update trash count badge)
```

No component knows about any other. The button doesn't know about models. The model doesn't know about the button. The command handler doesn't know about selection mode. Selection mode exit uses the GAction — not the bus.

---

## What gets replaced

| Current | Replaced by |
|---------|-------------|
| `std::sync::mpsc` channel | `glib::MainContext::channel` (push-based, unbounded) |
| `application.rs` idle loop (120 lines of routing) | Event translator (thin 1:1 mapping) + per-component subscriptions |
| `ModelRegistry` (100 lines, 8 methods) | Direct subscriptions via `EventBus` |
| `ActionContext` struct | `bus: &EventBus` — 2 params instead of 7 |
| `exit_selection` passthrough | `PhotoGridView` subscribes to `Trashed`/`Deleted`, activates `view.exit-selection` GAction |
| `library` + `tokio` in every action handler | `CommandDispatcher` — single component owns both, routes to `CommandHandler` impls |
| `registry.on_trashed()` calls in action handlers | `tx.send(TrashRequested { ids })` — handler emits intent only |
| `win.album-created` GAction hack | `AppEvent::AlbumCreated` — sidebar subscribes directly |

---

## What stays

| Component | Why |
|-----------|-----|
| `win.show-toast` GAction | Simple fire-and-forget UI notification |
| `view.zoom-in/out` GActions | View-scoped state, not cross-component |
| `view.enter/exit-selection` GActions | View-scoped state — selection mode is not an application event |
| `LibraryEvent` enum | Library-layer type — library must not depend on `AppEvent` |

---

## Subscribers

Each component subscribes to the events it cares about:

| Subscriber | Events consumed |
|------------|----------------|
| `CommandDispatcher` | `TrashRequested`, `RestoreRequested`, `DeleteRequested`, `FavoriteRequested`, `RemoveFromAlbumRequested` → executes library calls, emits result events |
| `PhotoGridModel` | `ThumbnailReady`, `FavoriteChanged`, `Trashed`, `Restored`, `Deleted`, `AssetSynced`, `AssetDeletedRemote`, `AlbumMediaChanged` |
| `Sidebar` | `SyncStarted`, `SyncProgress`, `SyncComplete`, `ThumbnailDownloadProgress`, `ThumbnailDownloadsComplete`, `ImportProgress`, `ImportComplete`, `AlbumCreated`, `AlbumRenamed`, `AlbumDeleted` |
| `PhotoGridView` | `Trashed`, `Deleted` → activates `view.exit-selection` GAction |
| `PeopleGrid` | `PeopleSyncComplete` → reloads |
| `Application` | `Ready`, `ShutdownComplete`, `Error` → lifecycle + error toasts |

---

## Migration strategy

Incremental, one event at a time. Each step is a single PR:

### Phase 1: Infrastructure
- Create `src/app_event.rs` with `AppEvent` enum
- Create `EventBus` with `glib::MainContext::channel` + fan-out dispatcher
- Create event translator in `application.rs` (LibraryEvent → AppEvent)
- Keep the idle loop as fallback for unmigrated events

### Phase 2: Thumbnail events
- Migrate `ThumbnailReady` — `PhotoGridModel` subscribes via bus
- Remove `ThumbnailReady` arm from idle loop
- Remove `ModelRegistry::on_thumbnail_ready()`

### Phase 3: Command dispatcher + media commands
- Create `CommandHandler` trait and `CommandDispatcher`
- Implement `TrashCommand`, `RestoreCommand`, `DeleteCommand`, `FavoriteCommand`
- Action handlers emit `*Requested` instead of calling library directly
- Remove `ModelRegistry::on_favorite_changed()`, `on_trashed()`, `on_deleted()`
- `PhotoGridView` subscribes to `Trashed`/`Deleted` → activates `view.exit-selection`

### Phase 4: Sync and import events
- Sidebar subscribes directly to sync/import events
- Remove sync/import routing from idle loop

### Phase 5: Album events + commands
- Implement `AddToAlbumCommand`, `RemoveFromAlbumCommand`
- Sidebar subscribes to album events
- Remove `win.album-created` GAction hack

### Phase 6: Cleanup
- Remove `ModelRegistry` entirely
- Remove idle loop (reduce to translator only)
- Remove `ActionContext` struct — handlers take `bus: &EventBus` only

---

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| `glib::MainContext::channel` is unbounded | Photo apps don't generate unbounded events; sync bursts are ~100 events max |
| Fan-out dispatcher iterates all subscribers for every event | Subscribers do a cheap `match` and ignore irrelevant events; ~6 subscribers total |
| `AppEvent` must be `Clone` for the command dispatcher | `MediaItem` and `MediaId` are already `Clone`; `ImportSummary` and `AlbumId` need `#[derive(Clone)]` |
| Migration breaks existing functionality | Incremental — one event at a time, old path as fallback |
| Circular event loops (handler emits event, subscriber handles it, emits again) | Convention: command handlers emit result events only, subscribers never emit commands in response |
| Command handler errors silently swallowed | Every `CommandHandler::execute` must send `AppEvent::Error` on failure — enforced by code review |

---

## Files affected (full migration)

| File | Change |
|------|--------|
| `src/app_event.rs` | **New** — `AppEvent` enum (commands + results) |
| `src/event_bus.rs` | **New** — `EventBus` (glib channel + fan-out subscriber registry) |
| `src/commands/mod.rs` | **New** — `CommandHandler` trait + `CommandDispatcher` |
| `src/commands/trash.rs` | **New** — `TrashCommand` |
| `src/commands/restore.rs` | **New** — `RestoreCommand` |
| `src/commands/delete.rs` | **New** — `DeleteCommand` |
| `src/commands/favorite.rs` | **New** — `FavoriteCommand` |
| `src/commands/add_to_album.rs` | **New** — `AddToAlbumCommand` |
| `src/commands/remove_from_album.rs` | **New** — `RemoveFromAlbumCommand` |
| `src/main.rs` | Create `EventBus`, pass to application |
| `src/application.rs` | Replace idle loop with event translator (LibraryEvent → AppEvent); subscribe for lifecycle + errors |
| `src/ui/model_registry.rs` | **Delete** — replaced by direct subscriptions |
| `src/ui/photo_grid/model.rs` | Subscribe to bus in constructor |
| `src/ui/photo_grid/action_bar.rs` | Replace `library` + `tokio` + `registry` + `exit_selection` with `bus` |
| `src/ui/photo_grid/actions.rs` | Replace `ActionContext` with `bus` |
| `src/ui/photo_grid.rs` | Pass `bus` instead of `registry`; subscribe for `Trashed`/`Deleted` → GAction |
| `src/ui/sidebar.rs` | Subscribe to bus in constructor |
| `src/ui/window.rs` | Remove `win.album-created` action, pass `bus` to components |
| `src/library/event.rs` | **Unchanged** — `LibraryEvent` stays as library-layer type |
| `src/library/sync.rs` | Migrate `std::sync::mpsc::Sender` → `glib::Sender<LibraryEvent>` |
| `src/library/importer.rs` | Migrate `std::sync::mpsc::Sender` → `glib::Sender<LibraryEvent>` |
| `src/library/thumbnailer.rs` | Migrate `std::sync::mpsc::Sender` → `glib::Sender<LibraryEvent>` |
| `src/library/providers/*.rs` | Migrate `std::sync::mpsc::Sender` → `glib::Sender<LibraryEvent>` |

# Design: Centralised Event Bus (#230)

**Status:** Proposed
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

Replace the single-consumer `mpsc` channel with a `tokio::sync::broadcast` channel. Each component subscribes directly to the events it cares about.

```
┌──────────────────────────────────────────────────────┐
│                    Library Backend                     │
│  ImportJob · SyncManager · Thumbnailer                │
│                      │                                │
│           broadcast::Sender<AppEvent>                 │
└──────────────────────────────────────────────────────┘
                       │
              ┌────────┼────────┬────────┬────────┐
              ▼        ▼        ▼        ▼        ▼
          PhotoGrid  Sidebar  People   Album    App
          Model      Status   Grid     Grid    (lifecycle)
```

### AppEvent enum

Unifies library events and UI events into a single typed enum:

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

    // ── Commands (UI intent → LibraryCommandHandler) ──
    TrashRequested { ids: Vec<MediaId> },
    RestoreRequested { ids: Vec<MediaId> },
    DeleteRequested { ids: Vec<MediaId> },
    FavoriteRequested { ids: Vec<MediaId>, state: bool },
    RemoveFromAlbumRequested { album_id: AlbumId, ids: Vec<MediaId> },

    // ── Results (LibraryCommandHandler → subscribers) ─
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

    // ── UI state ─────────────────────────────────────
    ExitSelectionMode,
}
```

Key changes from `LibraryEvent`:
- `FavoriteChanged`, `Trashed`, `Restored`, `Deleted` carry `Vec<MediaId>` (batch operations)
- `ExitSelectionMode` is a UI event — no library involvement
- `AppEvent` must be `Clone` (required by `broadcast`)
- `MediaItem` in `AssetSynced` must be `Clone`

### Design principle: self-contained components

**Event handlers must live inside the component that owns the behaviour, never in a parent.**

Parent components (`window.rs`, `application.rs`) are responsible for **assembly only** — creating child components and placing them in the layout. They must never route events to children or wire callbacks between siblings. Each component subscribes to the bus in its own constructor and handles its own events internally.

This ensures separation of concerns: adding a new event or changing how a component reacts to an event requires modifying only that component's file, not the parent that assembled it.

```rust
// ✅ CORRECT — component subscribes internally
let sidebar = MomentsSidebar::new(bus.clone());
// Done. Parent has no knowledge of what events sidebar handles.

// ❌ WRONG — parent routes events to child
let sidebar = MomentsSidebar::new();
// ...later in an idle loop or callback in window.rs:
match event {
    SyncStarted => sidebar.show_sync_started(),  // parent knows too much
    AlbumCreated { id, name } => sidebar.add_album(id, &name),
}
```

Every component constructor takes `bus: broadcast::Sender<AppEvent>` and calls `bus.subscribe()` internally to create its own receiver. When the component is dropped, the weak ref fails to upgrade and the polling stops automatically — no manual cleanup.

```rust
// Self-contained component pattern:
impl MomentsSidebar {
    pub fn new(bus: broadcast::Sender<AppEvent>) -> Self {
        let sidebar = Self { /* build UI */ };

        let mut rx = bus.subscribe();
        let weak = sidebar.downgrade();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            let Some(s) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            while let Ok(event) = rx.try_recv() {
                match event {
                    AppEvent::SyncStarted => s.show_sync_started(),
                    AppEvent::SyncProgress { .. } => s.show_sync_progress(..),
                    AppEvent::AlbumCreated { id, name } => s.add_album(id, &name),
                    AppEvent::ImportProgress { .. } => s.show_upload_progress(..),
                    _ => {}
                }
            }
            glib::ControlFlow::Continue
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
| `PhotoGridView::new(lib, tk, bus)` | Selection exit events | Yes |
| `ImportDialog::new(bus)` | Import progress/complete | Yes |

`window.rs` becomes pure assembly — create components, place in layout, done.

### Subscription polling

Each component creates its own receiver and polls it via `glib::timeout_add_local`:

```rust
impl PhotoGridModel {
    pub fn subscribe(self: &Rc<Self>, bus: &broadcast::Sender<AppEvent>) {
        let mut rx = bus.subscribe();
        let model = Rc::downgrade(self);

        glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            let Some(model) = model.upgrade() else {
                return glib::ControlFlow::Break;
            };
            while let Ok(event) = rx.try_recv() {
                match event {
                    AppEvent::ThumbnailReady { media_id } => model.on_thumbnail_ready(&media_id),
                    AppEvent::FavoriteChanged { ids, is_favorite } => {
                        for id in &ids {
                            model.on_favorite_changed(id, is_favorite);
                        }
                    }
                    AppEvent::Trashed { ids } | AppEvent::Deleted { ids } => {
                        for id in &ids {
                            model.remove_item(id);
                        }
                    }
                    AppEvent::AssetSynced { item } => model.on_asset_synced(&item),
                    _ => {} // Ignore events this component doesn't care about
                }
            }
            glib::ControlFlow::Continue
        });
    }
}
```

### Command / result event pattern

Events are split into two categories:

- **Command events** (`*Requested`) — UI intent. Emitted by buttons. Carry the minimum data the UI can resolve (e.g. selected IDs).
- **Result events** (`*Changed`, `*Completed`) — outcomes. Emitted by the command handler after the library operation succeeds. Consumed by models, sidebar, selection controller.

This separates concerns cleanly: **UI resolves UI state → command handler does library work → result event drives all downstream effects.**

#### Command handler: trait-based dispatch

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
    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &broadcast::Sender<AppEvent>,
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

    async fn execute(&self, event: AppEvent, library: &Arc<dyn Library>, bus: &Sender<AppEvent>) {
        let AppEvent::TrashRequested { ids } = event else { return };
        if library.trash(&ids).await.is_ok() {
            let _ = bus.send(AppEvent::Trashed { ids });
        }
    }
}

// src/commands/favorite.rs
pub struct FavoriteCommand;

#[async_trait]
impl CommandHandler for FavoriteCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::FavoriteRequested { .. })
    }

    async fn execute(&self, event: AppEvent, library: &Arc<dyn Library>, bus: &Sender<AppEvent>) {
        let AppEvent::FavoriteRequested { ids, state } = event else { return };
        if library.set_favorite(&ids, state).await.is_ok() {
            let _ = bus.send(AppEvent::FavoriteChanged { ids, is_favorite: state });
        }
    }
}
```

The dispatcher iterates handlers — no match block, no routing logic:

```rust
// src/commands/dispatcher.rs
pub struct CommandDispatcher;

impl CommandDispatcher {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        bus: broadcast::Sender<AppEvent>,
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

        let mut rx = bus.subscribe();
        let bus_tx = bus.clone();

        glib::timeout_add_local(Duration::from_millis(16), move || {
            while let Ok(event) = rx.try_recv() {
                for handler in &handlers {
                    if handler.handles(&event) {
                        let lib = Arc::clone(&library);
                        let bus = bus_tx.clone();
                        let evt = event.clone();
                        tokio.spawn(async move {
                            handler.execute(evt, &lib, &bus).await;
                        });
                        break; // one handler per event
                    }
                }
            }
            glib::ControlFlow::Continue
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

#### Button handlers become trivial

Buttons resolve UI state (selection → IDs) and emit a command. Nothing else.

```rust
// Before (12+ clones, 3 closure layers, 25 lines):
fn wire_trash(btn, selection, library, tokio, registry, exit_selection) {
    let sel = selection.clone();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let reg = Rc::clone(registry);
    let exit = exit_selection.clone();
    btn.connect_clicked(move |_| {
        let ids = collect_selected_ids(&sel);
        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let reg = Rc::clone(&reg);
        let exit = exit.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tk.spawn(async move { lib.trash(&ids).await }).await;
            if let Ok(Ok(())) = result {
                for id in &ids { reg.on_trashed(&id, true); }
                exit.activate(None);
            }
        });
    });
}

// After (2 clones, 1 closure layer, 6 lines):
fn wire_trash(btn, selection, bus) {
    let sel = selection.clone();
    let bus = bus.clone();
    btn.connect_clicked(move |_| {
        let ids = collect_selected_ids(&sel);
        if !ids.is_empty() {
            let _ = bus.send(AppEvent::TrashRequested { ids });
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
    bus: &broadcast::Sender<AppEvent>,
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
├─ Button handler:
│   ids = collect_selected_ids(selection)     // [id_1, id_2, id_3]
│   bus.send(TrashRequested { ids })           // done, 2 lines
│
├─ LibraryCommandHandler receives TrashRequested:
│   library.trash(&ids).await                  // async DB call
│   bus.send(Trashed { ids })                  // emit result
│
├─ PhotoGridModel (Photos) receives Trashed:
│   for id in ids { self.remove_item(id) }     // items disappear
│
├─ PhotoGridModel (Trash) receives Trashed:
│   for id in ids { self.fetch_and_insert(id) } // items appear in trash
│
├─ SelectionModeController receives Trashed:
│   exit_selection.activate(None)              // exits selection mode
│
└─ Sidebar receives Trashed:
    (could update trash count badge)
```

No component knows about any other. The button doesn't know about models. The model doesn't know about the button. The command handler doesn't know about selection mode.

---

## What gets replaced

| Current | Replaced by |
|---------|-------------|
| `std::sync::mpsc` channel | `tokio::sync::broadcast` channel |
| `application.rs` idle loop (120 lines of routing) | Per-component `subscribe()` with 16ms poll |
| `ModelRegistry` (100 lines, 8 methods) | Direct subscriptions — each model subscribes |
| `ActionContext` struct | `bus: broadcast::Sender<AppEvent>` — 2 params instead of 7 |
| `exit_selection` passthrough | `SelectionModeController` subscribes to `Trashed`/`Deleted` |
| `library` + `tokio` in every action handler | `CommandDispatcher` — single component owns both, routes to `CommandHandler` impls |
| `registry.on_trashed()` calls in action handlers | `bus.send(TrashRequested { ids })` — handler emits intent only |
| `win.album-created` GAction hack | `AppEvent::AlbumCreated` — sidebar subscribes directly |

---

## What stays

| Component | Why |
|-----------|-----|
| `win.show-toast` GAction | Simple fire-and-forget UI notification, not an event |
| `view.zoom-in/out` GActions | View-scoped state, not cross-component |
| `view.enter/exit-selection` GActions | View-scoped state, but `ExitSelectionMode` event replaces the passthrough pattern |
| `LibraryEvent` enum | Stays as the library-level event type; `AppEvent` wraps/replaces it at the application level |

---

## Subscribers

Each component subscribes to the events it cares about:

| Subscriber | Events consumed |
|------------|----------------|
| `LibraryCommandHandler` | `TrashRequested`, `RestoreRequested`, `DeleteRequested`, `FavoriteRequested`, `RemoveFromAlbumRequested` → executes library calls, emits result events |
| `PhotoGridModel` | `ThumbnailReady`, `FavoriteChanged`, `Trashed`, `Restored`, `Deleted`, `AssetSynced`, `AssetDeletedRemote`, `AlbumMediaChanged` |
| `Sidebar` | `SyncStarted`, `SyncProgress`, `SyncComplete`, `ThumbnailDownloadProgress`, `ThumbnailDownloadsComplete`, `ImportProgress`, `ImportComplete`, `AlbumCreated`, `AlbumRenamed`, `AlbumDeleted` |
| `SelectionModeController` | `Trashed`, `Deleted`, `ExitSelectionMode` → exits selection mode |
| `PeopleGrid` | `PeopleSyncComplete` → reloads |
| `Application` | `Ready`, `ShutdownComplete`, `Error` → lifecycle only |

---

## Migration strategy

Incremental, one event at a time. Each step is a single PR:

### Phase 1: Infrastructure
- Create `src/app_event.rs` with `AppEvent` enum
- Create `broadcast::channel` in `main.rs`, pass `Sender` to library and `Sender` to UI components
- Add `subscribe()` method to `PhotoGridModel`
- Keep the idle loop as fallback for unmigrated events

### Phase 2: Thumbnail events
- Migrate `ThumbnailReady` to broadcast
- `PhotoGridModel::subscribe()` handles it directly
- Remove `ThumbnailReady` arm from idle loop
- Remove `ModelRegistry::on_thumbnail_ready()`

### Phase 3: Media state events
- Migrate `FavoriteChanged`, `Trashed`, `Deleted`
- Action handlers emit `AppEvent` instead of calling `registry`
- Remove `ModelRegistry::on_favorite_changed()`, `on_trashed()`, `on_deleted()`
- Add `ExitSelectionMode` event, selection controller subscribes

### Phase 4: Sync and import events
- Migrate `SyncStarted/Progress/Complete`, `ImportProgress/Complete`
- Sidebar subscribes directly
- Remove sync/import routing from idle loop

### Phase 5: Album events
- Migrate `AlbumCreated/Renamed/Deleted/MediaChanged`
- Sidebar subscribes directly
- Remove `win.album-created` GAction hack

### Phase 6: Cleanup
- Remove `ModelRegistry` entirely
- Remove idle loop (or reduce to lifecycle-only)
- Remove `ActionContext` struct — handlers take `bus: Sender<AppEvent>` instead

---

## Risks and mitigations

| Risk | Mitigation |
|------|------------|
| `broadcast` receivers can lag | Set capacity to 256; handle `RecvError::Lagged` by logging and continuing |
| Multiple idle callbacks (one per subscriber) | Each polls at 16ms, same as current; total CPU cost similar |
| `AppEvent` must be `Clone` | `MediaItem` and `MediaId` are already `Clone`; `ImportSummary` needs `#[derive(Clone)]` |
| Migration breaks existing functionality | Incremental — one event at a time, old path as fallback |
| Circular event loops (handler emits event, subscriber handles it, emits again) | Convention: handlers emit events, subscribers never emit in response |

---

## Channel capacity

`broadcast::channel(256)` — based on:
- Sync can emit ~100 `AssetSynced` events in a burst
- Thumbnail generation emits one event per asset
- 256 provides headroom for burst + normal flow
- `Lagged` errors mean a subscriber fell behind — log and continue (stale thumbnails will be caught on next scroll)

---

## Files affected (full migration)

| File | Change |
|------|--------|
| `src/app_event.rs` | **New** — `AppEvent` enum (commands + results) |
| `src/commands/mod.rs` | **New** — `CommandHandler` trait + `CommandDispatcher` |
| `src/commands/trash.rs` | **New** — `TrashCommand` |
| `src/commands/restore.rs` | **New** — `RestoreCommand` |
| `src/commands/delete.rs` | **New** — `DeleteCommand` |
| `src/commands/favorite.rs` | **New** — `FavoriteCommand` |
| `src/commands/add_to_album.rs` | **New** — `AddToAlbumCommand` |
| `src/commands/remove_from_album.rs` | **New** — `RemoveFromAlbumCommand` |
| `src/main.rs` | Create `broadcast::channel`, pass sender |
| `src/application.rs` | Remove idle loop routing (120 lines), pass sender to library |
| `src/ui/model_registry.rs` | **Delete** — replaced by direct subscriptions |
| `src/ui/photo_grid/model.rs` | Add `subscribe()` method |
| `src/ui/photo_grid/action_bar.rs` | Replace `registry` + `exit_selection` with `bus` |
| `src/ui/photo_grid/actions.rs` | Replace `ActionContext` with `bus` |
| `src/ui/photo_grid.rs` | Pass `bus` instead of `registry`, subscribe selection controller |
| `src/ui/sidebar.rs` | Add `subscribe()` for sync/import/album events |
| `src/ui/window.rs` | Remove `win.album-created` action, pass `bus` to components |
| `src/library/event.rs` | Keep as-is or merge into `AppEvent` |
| `src/library/sync.rs` | Send `AppEvent` instead of `LibraryEvent` |
| `src/library/importer.rs` | Send `AppEvent` instead of `LibraryEvent` |
| `src/library/thumbnailer.rs` | Send `AppEvent` instead of `LibraryEvent` |
| `src/library/providers/*.rs` | Send `AppEvent` instead of `LibraryEvent` |

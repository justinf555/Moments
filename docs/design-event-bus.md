# Design: Centralised Event Bus (#230)

**Status:** Implemented (phase 1–6 complete), future evolution planned (#518)
**Issue:** [#230](https://github.com/justinf555/Moments/issues/230)

---

## Current Architecture (implemented)

The event bus provides push-based fan-out delivery of `AppEvent` values to
all subscribers on the GTK main thread. Events are produced from any thread
via the `Send + Clone` `EventSender` and delivered via `glib::idle_add_once`.

```
┌──────────────────────────────────────────────────────┐
│                    Library Backend                     │
│  ImportJob · SyncManager · Thumbnailer                │
│                      │                                │
│          Sender<LibraryEvent>  (std::sync::mpsc)      │
└──────────────────────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│         Event Translator (application.rs)             │
│                                                       │
│  LibraryEvent → AppEvent (1:1 mapping)               │
│  Runs on glib::timeout_add_local (16ms poll)         │
└──────────────────────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│                    EventBus                            │
│          mpsc channel + glib::idle_add_once            │
│                      │                                │
│  Thread-local subscriber list with fan-out            │
│  All subscribers receive every event (match to filter)│
└──────────────────────────────────────────────────────┘
              │        │        │        │
              ▼        ▼        ▼        ▼
          PhotoGrid  Sidebar  Command   Viewers
          Model      Status   Dispatch
```

### Key components

| File | Role |
|------|------|
| `src/event_bus.rs` | `EventBus`, `EventSender`, `Subscription` (RAII unsubscribe), thread-local subscriber list |
| `src/app_event.rs` | `AppEvent` enum — commands, results, lifecycle (~30 variants) |
| `src/commands/dispatcher.rs` | `CommandDispatcher` — routes `*Requested` events to `CommandHandler` impls on Tokio |
| `src/commands/*.rs` | Individual command handlers (trash, restore, delete, favorite, album operations) |
| `src/library/event.rs` | `LibraryEvent` — library-layer event type, sent via `mpsc` |

### Subscriber contract

- Subscriptions can be created and dropped at any time, including during
  event dispatch (e.g. from `WidgetImpl::realize` / `unrealize`)
- Drops during dispatch are deferred via `PENDING_REMOVALS` and flushed
  after the dispatch loop (#512)
- `Subscription` is `!Send` — must be dropped on the GTK thread

### Widget lifecycle pattern (#512)

All widget subscribers use `WidgetImpl::realize` / `unrealize` to manage
subscription lifetime:

| Component | realize | unrealize |
|-----------|---------|-----------|
| `PhotoGridView` | subscribe (exit-selection) + `model.activate()` | `model.deactivate()` + drop subscription |
| `AlbumGridView` | subscribe (album changes) + `reload_albums()` | drop subscription |
| `PhotoViewer` | subscribe (favorite rollback) | drop subscription |
| `VideoViewer` | subscribe (favorite rollback) | drop subscription |
| `MomentsSidebar` | subscribe (sync, import, trash count) | drop subscription |

Non-widget subscribers (`CommandDispatcher`, `MomentsApplication`) hold
subscriptions for the app lifetime.

### Command / result event pattern

```
User clicks "Delete"
│
├─ Button handler:
│   ids = collect_selected_ids(selection)
│   bus_sender.send(TrashRequested { ids })
│
├─ CommandDispatcher receives TrashRequested:
│   TrashCommand.execute() on Tokio:
│     library.trash(&ids).await
│     bus_sender.send(Trashed { ids })       // or Error on failure
│
├─ PhotoGridModel receives Trashed:
│   removes items from store
│
├─ PhotoGridView receives Trashed:
│   exits selection mode
│
└─ Sidebar receives Trashed:
    updates trash count badge
```

No component knows about any other. The button doesn't know about models.
The model doesn't know about selection mode.

---

## Known issues with current architecture

### 1. Translation loop (~140 lines of boilerplate)

`application.rs` contains a `LibraryEvent` → `AppEvent` translation loop
(lines 600–737) that maps variants 1:1. This exists because the library
sends `LibraryEvent` via a separate `mpsc` channel, but the bus uses
`AppEvent`. Most variants are identical field-for-field copies.

### 2. Coupled enum

All events (library results, UI commands, lifecycle) live in a single
`AppEvent` enum. Adding any event requires modifying this central type.
Both the library layer and UI layer depend on it.

### 3. Import dialog bypasses the bus

The import dialog progress is updated directly from the translation loop
instead of subscribing to the bus like every other component (#517).

---

## Future: Trait-based event bus (#518)

The planned evolution replaces the `AppEvent` enum with type-erased events,
enabling full decoupling between library and UI.

### Three-module architecture

```
┌──────────────────────────────────────────────────────┐
│                    Library Backend                     │
│  Defines event structs: ThumbnailReady, Trashed, etc  │
│  Sends directly via EventSender (no translation)      │
└──────────────────────────────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│                EventBus (standalone)                   │
│                                                       │
│  Type-erased transport: dyn Any + TypeId routing      │
│  Send input (any thread) / !Send output (GTK thread)  │
│  Knows nothing about library or UI                    │
│                                                       │
│  send<E: Event>(event: E)                             │
│  subscribe<E: Event>(Fn(&E)) -> Subscription          │
└──────────────────────────────────────────────────────┘
                       │
              ┌────────┼────────┐
              ▼        ▼        ▼
          PhotoGrid  Sidebar  Command
          Model      Status   Dispatch
```

### Event trait

```rust
// bus module — knows nothing about library or UI
pub trait Event: Any + Send + 'static {}

// library module — defines its own events
pub struct ThumbnailReady { pub media_id: MediaId }
impl Event for ThumbnailReady {}

pub struct TrashRequested { pub ids: Vec<MediaId> }
impl Event for TrashRequested {}
```

### What changes

| Current | Future |
|---------|--------|
| Single `AppEvent` enum (30+ variants) | Small structs, each `impl Event` |
| `LibraryEvent` → `AppEvent` translation loop | Library sends directly via bus |
| `match event { ... }` in each subscriber | `subscribe::<ThumbnailReady>(\|e\| ...)` per type |
| Separate `mpsc` channel for library events | Single bus, library uses `EventSender` directly |
| Adding event = modify central enum | Adding event = one struct + `impl Event` |

### Trade-offs

| Gain | Cost |
|------|------|
| Library and UI evolve independently | Loss of exhaustive `match` checking |
| No translation loop | Events scattered across modules |
| UI-to-UI events work without changes | Runtime `TypeId` matching (negligible) |
| Adding events doesn't touch existing code | More verbose subscriber setup (`Vec<Subscription>`) |

### Why three modules?

The library can't own `subscribe` because subscriber closures hold `!Send`
GTK widget references. The bus bridges `Send` events from Tokio to `!Send`
subscribers on the GTK thread. Neither the library nor the UI should own
this thread-crossing logic.

### Entry point

```rust
fn main() {
    let tokio = tokio::runtime::Runtime::new();
    let bus = EventBus::new();
    let library = LibraryFactory::create(bundle, config, bus.sender());
    let app = MomentsApplication::new(bus, library);
    app.run();
}
```

---

## Related issues

- [#512](https://github.com/justinf555/Moments/issues/512) — Widget lifecycle (realize/unrealize) for subscriptions ✅
- [#434](https://github.com/justinf555/Moments/issues/434) — RAII unsubscribe ✅
- [#516](https://github.com/justinf555/Moments/issues/516) — Move library init to main (superseded by #518)
- [#517](https://github.com/justinf555/Moments/issues/517) — Import dialog self-contained with bus-based progress
- [#518](https://github.com/justinf555/Moments/issues/518) — Trait-based event bus with three-module architecture

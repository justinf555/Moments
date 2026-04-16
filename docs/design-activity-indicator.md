# Activity Indicator Design

**Issue:** Replaces [#145](https://github.com/justinf555/Moments/issues/145) (sidebar status bar)
**Supersedes:** `docs/design-sidebar-status-bar.md`
**Status:** Proposed
**Date:** 2026-04-16

## Overview

Replace the sidebar bottom sheet status bar with a compact **activity indicator** in the sidebar header bar. A spinner/icon shows current state at a glance; a popover gives detail on click. Driven by two GObject clients: `SyncClient` (new) and `ImportClient` (reworked).

## Problem

The current bottom sheet takes up vertical space for low-value information. It conflates UI presentation with state management — the sidebar directly subscribes to raw events and manages state transitions itself. The sync engine has no proper client-layer abstraction, and the `ImportClient` doesn't follow the v2 GObject pattern.

## Design

### Visual: sidebar header bar

```
┌──────────────────────────────────┐
│ [Spinner/Icon]  Moments     [=]  │   <- sidebar header
├──────────────────────────────────┤
│  Photos                          │
│  Favorites                       │
│  ...                             │
└──────────────────────────────────┘
```

The activity indicator sits at the **left of the sidebar header** (`pack_start`). The hamburger menu stays at the right (`pack_end`).

### Indicator states

| State | Visual | Popover content | Condition |
|-------|--------|----------------|-----------|
| **Idle (local)** | Hidden, no click target | — | Local library, nothing happening |
| **Idle (Immich, has synced)** | Hidden | "Synced 2 minutes ago" | Immich library, nothing happening |
| **Idle (Immich, never synced)** | Hidden | "Not yet synced" | First launch, pre-sync |
| **Syncing** | `gtk::Spinner` | "Syncing... 42 items processed" (live) | Stream has items being processed |
| **Importing** | `gtk::Spinner` | "Importing 3/10" + `gtk::ProgressBar` | Import pipeline running |
| **Syncing + Importing** | `gtk::Spinner` | Both lines stacked | Concurrent activity |
| **Sync complete** | `object-select-symbolic` (5s) | "Synced 42 items" / "Synced just now" | Processing finished, items > 0 |
| **Import complete** | `object-select-symbolic` (5s) | "5 imported, 2 skipped" summary | Import finished |
| **Sync error** | `dialog-warning-symbolic` (static) | Error message, e.g. "Authentication failed" | Sync connection/auth failure |
| **Offline** | `network-offline-symbolic` (static) | "Server unreachable" + "Last synced 10m ago" | Server not reachable (Immich only) |

### Sync cycle visibility

The Immich sync cycle is: wake up -> connect to stream -> read items -> process -> sleep.

- **Connecting / checking stream** -> silent, no indicator
- **Stream has items, processing** -> spinner ("Syncing... N items")
- **Processing done, items > 0** -> tick for 5s, then hidden
- **Stream empty (nothing new)** -> stay hidden, update `last_synced_at` silently
- **Connection failed** -> error or offline icon

The key distinction: we do NOT show a spinner just for checking. Only when items are actually being processed.

### Priority when concurrent

Spinner covers both sync and import. The popover shows separate lines for each active operation. When both complete, the tick shows for 5s based on whichever finishes last.

Error/offline states persist until the next successful sync attempt — they are not overridden by import activity (import still shows its line in the popover alongside the error).

## Widget: ActivityIndicator

Self-contained GObject widget. The sidebar does `header.pack_start(&indicator)` and nothing else.

### Structure

```
ActivityIndicator (GObject widget)
+-- gtk::MenuButton (flat style, no dropdown arrow)
    +-- child: gtk::Stack [crossfade, 200ms]
    |   +-- "active"  -> gtk::Spinner
    |   +-- "status"  -> gtk::Image (tick / error / offline)
    |   +-- "hidden"  -> empty gtk::Box
    +-- popover: gtk::Popover
        +-- gtk::Box [vertical, spacing=8]
            +-- sync_row: gtk::Box [horizontal]
            |   +-- gtk::Image (sync icon)
            |   +-- gtk::Label (sync status text)
            +-- import_row: gtk::Box [horizontal]
            |   +-- gtk::Image (import icon)
            |   +-- gtk::Label (import status text)
            |   +-- gtk::ProgressBar (visible when importing)
            +-- error_row: gtk::Box [horizontal]
                +-- gtk::Image (warning icon)
                +-- gtk::Label (error detail)
```

Rows are shown/hidden based on state. When all rows are hidden, the button itself is hidden.

### Inputs

The widget holds references to `SyncClient` and `ImportClient` and binds to their GObject properties. All state logic lives inside the widget — it observes property changes and updates the stack/popover accordingly.

```rust
impl ActivityIndicator {
    pub fn new(sync_client: &SyncClient, import_client: &ImportClient) -> Self;
}
```

### State transitions

```
                    +----------+
           +------->|  Hidden  |<-----------+
           |        | (idle)   |            |
           |        +----+-----+            |
           |             |                  |
      5s timeout    SyncProcessing     ImportStarted
      (items > 0)   or ImportStarted        |
           |             |                  |
           |             v                  |
           |        +----------+       +----------+
           |        | Spinner  |       | Spinner  |
           |        | (sync)   |       | (import) |
           |        +----+-----+       +----+-----+
           |             |                  |
           |        SyncComplete       ImportComplete
           |        (items > 0)             |
           |             |                  |
           |             v                  v
           |        +----------+       +----------+
           +----- --| Tick     |       | Tick     |------+
                    | (5s)     |       | (5s)     |
                    +----------+       +----------+

  Error/Offline states:
      SyncError ---------> [error icon, persistent]
      SyncOffline -------> [offline icon, persistent]
      SyncConnected -----> clears error/offline
```

## SyncClient

New GObject singleton following the AlbumClient/PeopleClient v2 pattern.

### GObject properties

| Property | Type | Description |
|----------|------|-------------|
| `state` | `SyncState` enum | Idle, Syncing, Complete, Error, Offline |
| `items-processed` | `u32` | Running count during active sync |
| `last-synced-at` | `i64` | Unix timestamp of last successful sync |
| `error-message` | `String` | Human-readable error when state=Error/Offline |

### SyncState enum

```rust
enum SyncState {
    Idle,       // not syncing, no error
    Syncing,    // processing items from stream
    Complete,   // just finished (transient)
    Error,      // sync failed (auth, server error)
    Offline,    // server unreachable
}
```

### SyncEvent channel

Replaces the current `AppEvent::SyncStarted/SyncProgress/SyncComplete` events with a dedicated channel, matching the Album/People pattern:

```rust
enum SyncEvent {
    /// Stream connected and items are being processed.
    Processing { items: usize },
    /// Sync cycle complete.
    Complete { items: usize, errors: usize },
    /// Connection or auth error.
    Error { message: String },
    /// Server unreachable.
    Offline,
}
```

Note: there is no `Started` event. The sync engine emits `Processing` only when items actually arrive on the stream, not on connection. If the stream is empty, no event is emitted (the client stays idle, and `last_synced_at` is updated silently via `Complete { items: 0 }`).

### Configuration

```rust
impl SyncClient {
    pub fn configure(
        &self,
        events_rx: mpsc::UnboundedReceiver<SyncEvent>,
    );
}
```

The `listen` loop receives `SyncEvent` values and updates GObject properties, which the `ActivityIndicator` observes.

## ImportClient alignment

Rework the existing `ImportClient` to follow the v2 GObject pattern:

### Current issues
- Properties exist but are set via direct `imp()` access from the import closure
- No event channel — progress is marshalled manually via `glib::idle_add_once`
- No `configure()` pattern

### Target state
- `ImportEvent` channel from the import pipeline
- `configure(events_rx)` method
- `listen` loop updating properties from events
- Same GObject properties (state, current, total, imported, skipped, failed, elapsed_secs)

### ImportEvent

```rust
enum ImportEvent {
    Started { total: u32 },
    Progress { current: u32, imported: u32, skipped: u32, failed: u32 },
    Complete { imported: u32, skipped: u32, failed: u32, elapsed_secs: f64 },
}
```

## Removed

- `src/ui/sidebar/status_bar.rs` — entire file deleted
- `adw::BottomSheet` wrapping the sidebar toolbar view
- `AppEvent::SyncStarted`, `AppEvent::SyncProgress`, `AppEvent::SyncComplete` — replaced by `SyncEvent` channel
- Sidebar event bus subscriptions for sync events
- `StatusState` enum, priority state machine, idle timer in sidebar

## Implementation Phases

| Phase | Description | PR |
|-------|-------------|----|
| 1 | **SyncClient** — GObject singleton, `SyncEvent` channel, properties. Wire sync engine to emit `SyncEvent` instead of `AppEvent`. Register on `MomentsApplication`. | New client, backend wiring |
| 2 | **ImportClient alignment** — Introduce `ImportEvent` channel, `configure()` + `listen()` pattern. Remove manual `glib::idle_add_once` marshalling. | Rework existing client |
| 3 | **ActivityIndicator widget** — Self-contained widget binding SyncClient + ImportClient. Add to sidebar header. Remove `StatusBar`, `BottomSheet`, old event subscriptions. | UI swap, delete dead code |

Each phase is a mergeable PR. Phase 1 and 2 are independent and could be done in either order.

## Edge Cases

- **Local backend:** No `SyncClient` configured, indicator only shows import activity
- **Very fast sync (< 1s, few items):** Still shows tick for 5s — user sees something happened
- **Empty sync (0 items):** No spinner, no tick, just silent `last_synced_at` update
- **Import during sync error:** Spinner shows for import; error icon returns after import completes
- **Multiple rapid syncs:** Each non-empty sync resets the tick timer
- **App startup (Immich, pre-first-sync):** Popover shows "Not yet synced" if clicked
- **Sync error then recovery:** Next successful sync clears error state automatically

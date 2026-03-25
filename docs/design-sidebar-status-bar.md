# Sidebar Status Bar Design

**Issue:** [#145](https://github.com/justinf555/Moments/issues/145)
**Status:** Proposed
**Date:** 2026-03-25

## Overview

The sidebar bottom bar is currently hidden and only appears during uploads. This design makes it a **persistent status bar** that shows the current state of the library — idle, syncing, downloading thumbnails, or uploading — giving the user continuous visibility into what Moments is doing.

## Problem

Today, there's no indication that the app is actively syncing with the Immich server, downloading thumbnails, or otherwise working in the background. Photos "magically appear" in the grid with no feedback. Users can't tell if sync is stuck, in progress, or complete. The Import button is buried in the header bar with no persistent home.

## UX States

The bottom bar transitions between **five states**, displayed in priority order (highest wins when multiple activities overlap):

### State 1: Upload Active (highest priority)

User-initiated import in progress. This is the most important state because the user is actively waiting.

```
┌──────────────────────────┐
│                          │
│  ⬆  Uploading 45/234  ▴ │  ← compact bar (expandable)
└──────────────────────────┘

Expanded sheet:
┌──────────────────────────┐
│  Uploading 45 of 234     │
│  ████████░░░░  19%       │
│  12 skipped, 0 failed    │
└──────────────────────────┘
```

- **Icon:** `go-up-symbolic`
- **Bar text:** "{current}/{total}"
- **Expandable:** Yes — drag up for progress bar and detail counts
- Identical to the existing upload progress behavior

### State 2: Sync Active

Background sync stream is downloading asset/album/people records from the Immich server.

```
┌──────────────────────────┐
│                          │
│  ↻  Syncing...           │
└──────────────────────────┘
```

- **Icon:** `emblem-synchronizing-symbolic` (spinning if GTK supports it, static otherwise)
- **Bar text:** "Syncing..." or "Syncing... 1,432 items" if count is available
- **Expandable:** No — sync is fast and not user-initiated, minimal detail needed
- Appears when the sync stream is connected and processing records

### State 3: Thumbnails Downloading

Background thumbnail worker pool is downloading face/asset thumbnails after sync.

```
┌──────────────────────────┐
│                          │
│  ↓  Thumbnails 234/1,976 │
└──────────────────────────┘
```

- **Icon:** `folder-download-symbolic`
- **Bar text:** "Thumbnails {completed}/{total}"
- **Expandable:** No — informational only
- Appears after sync completes when thumbnails are being fetched
- Updates as each thumbnail downloads (throttled to avoid flooding the UI — update every 10th thumbnail or every 500ms, whichever comes first)

### State 4: Upload Complete (transient)

Shown for 5 seconds after an upload finishes, then transitions to idle.

```
┌──────────────────────────┐
│                          │
│  ✓  234 imported         │
└──────────────────────────┘
```

- **Icon:** `emblem-ok-symbolic`
- **Bar text:** "{count} imported" (with skip/fail counts if non-zero)
- Auto-reverts to idle after 5 seconds

### State 5: Idle (lowest priority, default)

No background activity. Shows the Import button and last sync time.

```
┌──────────────────────────────────────┐
│                                      │
│  [⬆ Import]          Synced 2m ago   │
└──────────────────────────────────────┘
```

- **Left:** Import button — triggers `app.import` action
- **Right:** Last sync timestamp — "Synced just now", "Synced 30s ago", "Synced 2m ago", "Synced 1h ago"
- Updated every 10 seconds via `timeout_add_local`
- For the local backend (no sync): just the Import button, no sync timestamp

## State Machine

```
                    ┌─────────┐
           ┌──────►│  Idle    │◄──────────────┐
           │       │ Import + │               │
           │       │ Sync time│               │
           │       └────┬─────┘               │
           │            │                     │
     5s timeout    SyncStarted          ImportComplete
           │            │                     │
           │            ▼                     │
           │       ┌─────────┐          ┌─────────┐
           │       │ Syncing  │    ┌───►│ Upload   │
           │       │          │    │    │ Active   │
           │       └────┬─────┘    │    └─────────┘
           │            │          │          ▲
           │       SyncComplete    │    ImportProgress
           │            │          │          │
           │            ▼          │    ImportStarted
           │       ┌─────────┐    │    (from any state)
           │       │Thumbnails│───┘
           │       │Downloading│
           │       └────┬─────┘
           │            │
           │     ThumbnailsComplete
           │            │
           │            ▼
           │       ┌─────────┐
           └───────│ Complete │
                   │(transient)│
                   └──────────┘
```

**Priority override:** Upload Active always takes precedence. If the user starts an import while sync or thumbnails are in progress, the bar immediately switches to Upload Active. When the upload finishes, it falls back to whatever background activity is still running (thumbnails, sync) or idle.

## New Library Events

| Event | Source | Data |
|-------|--------|------|
| `SyncStarted` | `SyncManager::run_sync` start | — |
| `SyncProgress` | `SyncManager::run_sync` every flush | `{ assets: usize, people: usize, faces: usize }` |
| `SyncComplete` | `SyncManager::run_sync` end | `{ assets: usize, people: usize, faces: usize, errors: usize }` |
| `ThumbnailProgress` | `ThumbnailDownloader` per download | `{ completed: usize, total: usize }` |
| `ThumbnailsComplete` | `ThumbnailDownloader` channel closed | `{ total: usize }` |

Existing events remain unchanged:
- `ImportProgress { current, total }` — upload progress
- `ImportComplete(ImportSummary)` — upload finished

## Implementation

### Bottom bar widget changes

Replace the current single-purpose upload progress bar with a `GtkStack` inside the bottom bar that switches between states:

```rust
// Bottom bar: stack of state-specific widgets
let bar_stack = gtk::Stack::new();
bar_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
bar_stack.set_transition_duration(200);

// Idle page: Import button + sync time label
let idle_box = gtk::Box::new(Horizontal, 8);
// ... import_btn + sync_label

// Sync page: icon + "Syncing..."
let sync_box = gtk::Box::new(Horizontal, 8);
// ... sync_icon + sync_label

// Thumbnail page: icon + "Thumbnails X/Y"
let thumb_box = gtk::Box::new(Horizontal, 8);
// ... thumb_icon + thumb_label

// Upload page: icon + "X/Y"
let upload_box = gtk::Box::new(Horizontal, 8);
// ... upload_icon + upload_label

// Complete page: icon + summary
let complete_box = gtk::Box::new(Horizontal, 8);
// ... check_icon + complete_label

bar_stack.add_named(&idle_box, Some("idle"));
bar_stack.add_named(&sync_box, Some("sync"));
bar_stack.add_named(&thumb_box, Some("thumbnails"));
bar_stack.add_named(&upload_box, Some("upload"));
bar_stack.add_named(&complete_box, Some("complete"));
```

### Sidebar API

```rust
impl MomentsSidebar {
    // Existing (renamed for clarity)
    pub fn show_upload_progress(&self, current: usize, total: usize);
    pub fn show_upload_complete(&self, summary: &ImportSummary);

    // New
    pub fn show_sync_started(&self);
    pub fn show_sync_progress(&self, assets: usize, people: usize, faces: usize);
    pub fn show_sync_complete(&self, assets: usize);
    pub fn show_thumbnail_progress(&self, completed: usize, total: usize);
    pub fn show_thumbnails_complete(&self, total: usize);
    pub fn set_idle(&self);  // called internally after transient states
}
```

### Sync time tracking

The sidebar stores the last sync completion timestamp and uses a 10-second timer to update the "Synced X ago" label:

```rust
// In sidebar imp:
pub last_synced_at: Cell<Option<i64>>,  // Unix timestamp
pub sync_timer: RefCell<Option<glib::SourceId>>,
```

The timer formats the elapsed time:
- < 10s: "Synced just now"
- < 60s: "Synced 30s ago"
- < 3600s: "Synced 2m ago"
- ≥ 3600s: "Synced 1h ago"

### Event wiring in application.rs

```rust
Ok(LibraryEvent::SyncStarted) => {
    if let Some(win) = win_for_idle.upgrade() {
        if let Some(sb) = win.sidebar() {
            sb.show_sync_started();
        }
    }
}
Ok(LibraryEvent::SyncComplete { assets, .. }) => {
    if let Some(win) = win_for_idle.upgrade() {
        if let Some(sb) = win.sidebar() {
            sb.show_sync_complete(assets);
        }
    }
}
Ok(LibraryEvent::ThumbnailProgress { completed, total }) => {
    if let Some(win) = win_for_idle.upgrade() {
        if let Some(sb) = win.sidebar() {
            sb.show_thumbnail_progress(completed, total);
        }
    }
}
// ... etc
```

## Files Changed

| File | Change |
|------|--------|
| `src/library/event.rs` | Add SyncStarted, SyncProgress, SyncComplete, ThumbnailProgress, ThumbnailsComplete variants |
| `src/library/sync.rs` | Emit new events at sync start/flush/end and from thumbnail downloader |
| `src/ui/sidebar.rs` | Replace bottom bar with GtkStack, add state methods, sync time timer |
| `src/application.rs` | Wire new events to sidebar methods |

## Implementation Phases

| Phase | Description | Scope |
|-------|-------------|-------|
| 1 | Persistent idle bar with Import button | Bottom bar always visible, import button, no sync status yet |
| 2 | Sync events + sync status display | SyncStarted/Complete events, "Syncing..." state, "Synced X ago" timer |
| 3 | Thumbnail download progress | ThumbnailProgress events, thumbnail count display |
| 4 | Priority state machine | Handle overlapping states correctly (upload overrides sync/thumbnails) |

## Edge Cases

- **First launch (no sync yet):** Idle state shows "Import" button only, no "Synced" time
- **Sync error:** Show "Sync failed" briefly, then revert to idle with last successful sync time
- **Very fast sync (< 1s):** Skip the "Syncing..." state entirely — go straight to idle or thumbnails
- **Offline:** Don't show "Syncing..." when the server is unreachable — let the sync error handling deal with it
- **Multiple rapid syncs:** Each sync resets the "Synced X ago" timer
- **Upload during sync:** Upload state takes priority; sync continues in background and state falls back after upload completes

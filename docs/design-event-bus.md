# Design: Centralised Event Bus (#230) — SUPERSEDED

**Status:** Removed in [#580](https://github.com/justinf555/Moments/pull/598) (epic [#587](https://github.com/justinf555/Moments/issues/587)).
**Original issue:** [#230](https://github.com/justinf555/Moments/issues/230)

The single-bus / single-`AppEvent`-enum architecture described in earlier
revisions of this document has been replaced by per-service event channels
plus GObject signals on the client singletons. This file is kept as a pointer
for git-archaeology readers.

---

## What replaced it

### Per-service `EventEmitter<T>` channels (`src/event_emitter.rs`)

Each library service owns its own typed event emitter:

| Service | Event type | Variants |
|---|---|---|
| `MediaService` | `MediaEvent` | `Added(Vec<MediaId>)`, `Updated(Vec<MediaId>)`, `Removed(Vec<MediaId>)` |
| `ThumbnailService` | `ThumbnailEvent` | `Ready(MediaId)` |
| `AlbumService` | `AlbumEvent` | `AlbumAdded(AlbumId)`, `AlbumUpdated(AlbumId)`, `AlbumRemoved(AlbumId)`, `AlbumMediaChanged(AlbumId)` |
| `FacesService` | `FacesEvent` | `PersonAdded`, `PersonUpdated`, `PersonRemoved`, `PersonMediaChanged` |

Subscribers call `service.subscribe()` to get an `mpsc::UnboundedReceiver`
on the Tokio side; events fan out to every live subscriber. No translation
loop, no polling timer.

### GObject signals on client singletons

UI-side fan-out happens through GObject signals on the client GObjects:

- `MediaClientV2`: `items-trashed(u32)`, `items-restored(u32)`, `items-deleted(u32)`, `favorite-changed(u32, bool)`
- `AlbumClientV2`: `album-media-changed(String)`, `album-deleted(String)`
- `ImportClient` / `SyncClient`: progress / state notifications via `glib::Property`

Widgets connect via `connect_closure` in `realize` and disconnect in
`unrealize`. No Subscription type is needed.

### Commands flow directly

UI → client method (`MediaClientV2::trash(ids)`, `AlbumClientV2::create_album(name, media_ids)`, …) → library service (Tokio) → service emits its own event. There is no `CommandDispatcher`, no `*Requested` events, no command handlers.

Errors surface as toasts via `crate::client::show_error_toast` directly from
the client method on failure — no centralised `AppEvent::Error`
subscription.

---

## Migration history

- [#576](https://github.com/justinf555/Moments/pull/576) — deleted dead `AppEvent` variants (Ready, ShutdownComplete, ImportProgress, ImportComplete).
- [#577](https://github.com/justinf555/Moments/pull/577) — UI-side error toasts via `crate::client::show_toast()`, not bus subscriptions.
- [#578](https://github.com/justinf555/Moments/pull/578) — `MediaClient` owned command dispatch (still on bus internally).
- [#579](https://github.com/justinf555/Moments/pull/579) — result fan-out via GObject signals.
- [#588](https://github.com/justinf555/Moments/pull/588) — `EventEmitter<T>` fan-out primitive + per-service event types.
- [#590](https://github.com/justinf555/Moments/pull/590) — `MediaClientV2` read path on the new channels.
- [#596](https://github.com/justinf555/Moments/pull/596) — `MediaClientV2` owned commands; `subscribe_commands` removed.
- [#597](https://github.com/justinf555/Moments/pull/597) — V1 read-path call sites flipped to V2; `items-deleted` consolidated on `on_media_removed`.
- [#598](https://github.com/justinf555/Moments/pull/598) — `EventBus`, `AppEvent`, `library/commands/`, `MediaClient` v1 deleted. Closes #580.

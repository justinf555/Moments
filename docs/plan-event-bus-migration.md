# Migration Plan: Event Bus Architecture Evolution — COMPLETED

**Status:** Superseded. The migration was reframed and completed under epic [#587](https://github.com/justinf555/Moments/issues/587), closing [#580](https://github.com/justinf555/Moments/issues/580).
**Original issue:** [#518](https://github.com/justinf555/Moments/issues/518)

The original plan (CQRS / `LibraryQuery` trait split / trait-based event bus) was
not the path taken. Instead the EventBus and `AppEvent` enum were removed
outright in favour of per-service `EventEmitter<T>` channels plus GObject
signals on the client singletons. See [`design-event-bus.md`](design-event-bus.md)
for the post-migration architecture.

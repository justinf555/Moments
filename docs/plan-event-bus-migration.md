# Migration Plan: Event Bus Architecture Evolution

**Issue:** [#518](https://github.com/justinf555/Moments/issues/518)
**Goal:** Move from a single `AppEvent` enum with a translation loop and command dispatcher to a trait-based event bus with CQRS separation.

---

## Phase 1: Eliminate the translation loop

Remove the `LibraryEvent` → `AppEvent` middleman. The library sends `AppEvent` directly via `EventSender` instead of going through a separate `mpsc` channel.

**What changes:**
- Library backends use `EventSender` instead of `mpsc::Sender<LibraryEvent>`
- `LibraryEvent` enum is deleted
- The ~140 line translation loop in `application.rs` is deleted
- The 16ms polling timer becomes unnecessary (bus is already push-based)

**Depends on:** #517 (import dialog must use the bus first, otherwise it still needs direct access in the translation loop)

---

## Phase 2: Library subscribes to commands (eliminate CommandDispatcher)

The library subscribes to `*Requested` events directly instead of routing through `CommandDispatcher`.

**What changes:**
- Library registers command handlers during initialisation
- `CommandDispatcher`, `CommandHandler` trait, and all command handler files are deleted
- Error handling moves into the library's command subscription

---

## Phase 3: Split Library trait into read-only queries + bus commands

Separate the `Library` trait into a read-only query interface and mutation commands.

**What changes:**
- New `LibraryQuery` trait (or similar) with read-only methods: `list_media`, `thumbnail_path`, `original_path`, `media_metadata`, `list_albums`, `list_people`, `library_stats`
- Mutation methods (`trash`, `restore`, `favorite`, `create_album`, etc.) removed from the trait — they're handled via bus commands from Phase 2
- UI components receive `Arc<dyn LibraryQuery>` instead of `Arc<dyn Library>`

---

## Phase 4: Trait-based events (replace AppEvent enum)

Replace the `AppEvent` enum with individual event structs and type-erased bus routing.

**What changes:**
- `Event` trait defined in the bus module
- Each event becomes its own struct with `impl Event`
- `EventBus` uses `TypeId` for routing — `subscribe::<T>()` / `send::<T>()`
- `AppEvent` enum is deleted
- All subscribers update from `match event { ... }` to typed `subscribe` calls

---

## Phase 5: Move library initialisation to main

Clean entry point with dependency injection.

**What changes:**
- `main.rs` creates Tokio runtime, event bus, and library
- `MomentsApplication` receives bus + query handle — doesn't create them
- `application.rs` shrinks to GTK lifecycle only (activate, shutdown, GActions)

---

## Suggested scheduling

| Phase | Size | Dependencies | Notes |
|-------|------|-------------|-------|
| 1 | M | #517 | Biggest immediate code reduction |
| 2 | S | Phase 1 | Small once phase 1 is done |
| 3 | M | Phase 2 | Trait split, update all UI call sites |
| 4 | L | Phase 3 | Touches every subscriber — do last |
| 5 | S | Phase 1 | Can be done anytime after phase 1 |

Phases 1 and 2 deliver the most value (eliminate boilerplate, remove middleman). Phase 3 is the architectural win (CQRS). Phase 4 is the largest change but is optional — the architecture works with or without it. Phase 5 is a small cleanup.

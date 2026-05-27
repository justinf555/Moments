# Design: LibraryContext and Application Wiring

**Issue:** TBD
**Status:** Proposed
**Date:** 2026-05-27

## Problem

`application/mod.rs` is 898 lines, dominated by `load_library_async` (a 270-line function doing 7 distinct startup concerns) and a struct holding 13 nullable runtime fields:

```rust
pub settings: OnceCell<gio::Settings>,
pub tokio: OnceCell<tokio::runtime::Handle>,
pub library: RefCell<Option<Arc<Library>>>,
pub import_client: RefCell<Option<crate::client::ImportClient>>,
pub album_client_v2: RefCell<Option<crate::client::AlbumClientV2>>,
pub people_client: RefCell<Option<crate::client::PeopleClientV2>>,
pub media_client_v2: RefCell<Option<crate::client::MediaClientV2>>,
pub sync_client: RefCell<Option<crate::client::SyncClient>>,
pub render_pipeline: RefCell<Option<Arc<crate::renderer::pipeline::RenderPipeline>>>,
pub immich_server_url: RefCell<Option<String>>,
pub purge_handle: RefCell<Option<tokio::task::JoinHandle<()>>>,
pub sync_handle: RefCell<Option<crate::sync::SyncHandle>>,
```

Symptoms:

- **Implicit init order.** Each call site has to know whether a given service has been populated yet. The ActivityIndicator timing bug came from a UI element constructed before its dependency client was set.
- **Runtime nullability everywhere.** Every access goes through `RefCell::borrow().as_ref().unwrap()`. Borrow panics and missing-initialization panics are not statically prevented.
- **No architectural boundary.** Any widget can write `MomentsApplication::default().library()` and reach directly into the domain, bypassing the client layer that exists for exactly that purpose.
- **Mixed concerns on one type.** `Application` holds GTK lifecycle, settings, the domain, UI-binding clients, background task handles, and one-off config strings.
- **Hard to test in isolation.** Anything wanting to exercise domain code through the application path needs a real `adw::Application`.

The reviewer report (`moments_repository_engineering_review.md`) flagged this as the highest-severity architectural finding. This document proposes a concrete refactor.

## Goals

1. Make GTK widgets *physically unable* to reach the domain except through clients (compile-time, not convention).
2. Replace runtime nullability with `OnceCell` everywhere that initializes-once.
3. Split startup into typed phases instead of one mega-function.
4. Make domain code testable without a GTK Application.
5. Keep the existing per-service `EventEmitter<T>` pattern and the existing client/widget GObject contract.

Non-goals:

- Introducing a service container / dependency-injection framework.
- Trait-abstracting library services (no current second implementation or test demand).
- Multi-window / per-window scoping (single-window app for the foreseeable future).

## Taxonomy

Five kinds of runtime objects, each with a distinct lifecycle and visibility:

| Kind | Distinguishing trait | Example | Owned by | Visibility |
|---|---|---|---|---|
| Domain / Infrastructure | App lifetime, holds primary state | `Library`, `RenderPipeline`, `tokio::Handle` | `LibraryContext` | `pub(in crate::application)` |
| Self-contained background task | App lifetime, no client holds a reference; configured via watch channel | `PurgeTask` — periodic trash auto-purge | `LibraryContext` (as `JoinHandle` + watch sender) | `pub(in crate::application)` |
| Long-running service with client API | App lifetime, paired Client invokes methods on it | `SyncEngine` ↔ `SyncClient` — HTTP client, polling loop, push/pull queues | Field on `Application` | `pub(in crate::application)` |
| Ephemeral per-operation work | Per call, built fresh and dropped | `ImportPipeline` — built, run, dropped on each `import()` | Internal to its paired Client | n/a (private impl) |
| UI Client (GObject facade) | App lifetime, presents domain operations as a GObject for widget binding | `MediaClientV2`, `AlbumClientV2`, `SyncClient`, `ImportClient` | Field on `Application` | `pub` |

**A note on language: "facade" vs "client".**

The UI Client layer is, architecturally, a *facade* in the GoF sense — a tailored interface over the `LibraryContext` subsystem, shaped for one particular caller (GTK widgets). The codebase calls these types `*Client*` for historical reasons and that naming convention is kept. When future facades over the same `LibraryContext` arrive for *different* callers (e.g. a D-Bus interface), those layers will naturally be named to reflect their role (e.g. `DBusFacade`) and the asymmetry is intentional — it distinguishes the GTK facade from the D-Bus facade and from D-Bus *callers* (which would themselves be "clients" in the network sense). The architectural noun is **facade**; the GTK-side concrete naming is **Client**.

**Decision rules:**

1. **Lifecycle test — long-running vs ephemeral:** does it hold state that persists across operations (HTTP client pool, last-sync cursor, in-flight queues)? Yes → long-running. No → ephemeral, owned inside its client.

2. **Reachability test — `LibraryContext` field vs `Application` field:** does any code outside the task itself need to invoke methods on it after startup (a paired Client driving it, a UI button cancelling it)? Yes → typed identity on `Application`. No → just a `JoinHandle` in `LibraryContext`.

If a background task later grows a UI consumer (e.g. a "Purge Now" button), it gets promoted to its own `Application` field with a paired Client. Until that happens, the simpler shape is correct.

## Proposed Architecture

### LibraryContext

A pure-Rust struct that owns the domain + infrastructure. Lives in `src/application/context.rs`. Module visibility is `pub(in crate::application)`, making it **unspellable outside the `application/` module tree**.

```rust
// src/application/context.rs
pub(in crate::application) struct LibraryContext {
    library: Arc<Library>,
    render_pipeline: Arc<RenderPipeline>,
    tokio: tokio::runtime::Handle,
    purge_handle: OnceCell<tokio::task::JoinHandle<()>>,
    config: AppConfig,
}

impl LibraryContext {
    pub(in crate::application) async fn build(
        bundle: Bundle,
        tokio: tokio::runtime::Handle,
        config: AppConfig,
    ) -> Result<Arc<Self>, OpenError> {
        let library = Library::open(bundle, config.mode, /* ... */).await?;
        let render_pipeline = Arc::new(RenderPipeline::new(/* ... */));
        Ok(Arc::new(Self {
            library, render_pipeline, tokio, config,
            purge_handle: OnceCell::new(),
        }))
    }

    // Accessors return concrete sub-services with minimal surface.
    pub(in crate::application) fn library(&self)         -> &Arc<Library>           { &self.library }
    pub(in crate::application) fn render_pipeline(&self) -> &Arc<RenderPipeline>    { &self.render_pipeline }
    pub(in crate::application) fn tokio(&self)           -> &tokio::runtime::Handle { &self.tokio }
    pub(in crate::application) fn media_service(&self)   -> Arc<MediaService>       { self.library.media().clone() }
    pub(in crate::application) fn album_service(&self)   -> Arc<AlbumService>       { self.library.albums().clone() }
    pub(in crate::application) fn thumbnail_service(&self) -> Arc<ThumbnailService> { self.library.thumbnails().clone() }
    // ...

    pub(in crate::application) fn start_purge_task(&self) { /* ... */ }
}
```

### MomentsApplication

Public surface shrinks to GTK plumbing + UI-binding clients.

```rust
mod imp {
    pub struct MomentsApplication {
        // GTK
        pub settings: OnceCell<gio::Settings>,

        // pub(in crate::application) — invisible to ui/, client/, library/
        pub library_context: OnceCell<Arc<LibraryContext>>,
        pub sync_engine: OnceCell<Arc<SyncEngine>>,

        // pub — UI access surface
        pub media_client: OnceCell<MediaClientV2>,
        pub album_client: OnceCell<AlbumClientV2>,
        pub people_client: OnceCell<PeopleClientV2>,
        pub import_client: OnceCell<ImportClient>,
        pub sync_client: OnceCell<SyncClient>,
    }
}

impl MomentsApplication {
    // Public — anything in ui/ may call these.
    pub fn media_client(&self)  -> &MediaClientV2  { self.imp().media_client.get().expect("client not initialized") }
    pub fn album_client(&self)  -> &AlbumClientV2  { self.imp().album_client.get().expect("client not initialized") }
    pub fn import_client(&self) -> &ImportClient   { self.imp().import_client.get().expect("client not initialized") }
    pub fn sync_client(&self)   -> Option<&SyncClient> { self.imp().sync_client.get() }
    // ...

    // pub(in crate::application) — wiring only.
    pub(in crate::application) fn library_context(&self) -> &Arc<LibraryContext> {
        self.imp().library_context.get().expect("context not initialized")
    }
}
```

13 mixed-shape nullable fields → 8 `OnceCell` fields, with privacy reflecting purpose.

### Construction convention

Every Client and every long-running service exposes exactly one constructor, named **`build`**, taking concrete minimal dependencies. It is called only from inside `application/`:

```rust
impl MediaClientV2 {
    pub fn build(media: Arc<MediaService>, thumbnails: Arc<ThumbnailService>) -> Self { /* ... */ }
}

impl SyncEngine {
    pub fn build(library: Arc<Library>, tokio: tokio::runtime::Handle, /* ... */) -> Arc<Self> { /* ... */ }
}
```

`build` is the canonical wiring entry point. The signature *is* the dependency contract.

Use a builder-method-chain only when the dependency list is large (3+) or async, with `.build()` as the terminal method. Either way, `build` is what wiring code calls.

### Type-system enforcement

UI code in `src/ui/` cannot construct any client because:

1. `LibraryContext` is `pub(in crate::application)` — the type cannot be named outside `application/`.
2. There is no public accessor on `Application` returning `Arc<Library>`, `Arc<MediaService>`, etc.
3. `Library::open` requires `Bundle`, `Database`, `MutationRecorder`, `OriginalResolver` — none of which UI code has any path to obtain.

Therefore the **only** code path that can construct `MediaClientV2::build(...)` is inside `application/`. A widget that tries will fail to compile because it cannot produce the constructor arguments.

This converts the existing convention ("widgets must use clients") into a compile-time invariant.

### Startup phases

Replace `load_library_async` with four phase functions, each returning a typed value the next phase consumes:

```rust
// src/application/startup.rs

pub(in crate::application) async fn start(
    app: &MomentsApplication,
    settings: gio::Settings,
) -> Result<(), StartupError> {
    let config = AppConfig::from_settings(&settings);
    let bundle = open_bundle(&config).await?;
    let tokio  = app.tokio_handle().clone();

    // Phase 1 — domain
    let ctx = LibraryContext::build(bundle, tokio, config).await?;

    // Phase 2 — long-running services (conditional on backend)
    let sync_engine = if ctx.config().has_immich() {
        Some(SyncEngine::build(
            ctx.library().clone(),
            ctx.tokio().clone(),
            /* recorder, resolver, http, ... */
        ).await?)
    } else {
        None
    };

    // Phase 3 — UI clients (minimal deps each)
    let media_client  = MediaClientV2::build(ctx.media_service(), ctx.thumbnail_service());
    let album_client  = AlbumClientV2::build(ctx.album_service());
    let people_client = PeopleClientV2::build(ctx.faces_service());
    let import_client = ImportClient::build(ctx.library().clone(), ctx.render_pipeline().clone(), ctx.tokio().clone());
    let sync_client   = sync_engine.as_ref().map(|e| SyncClient::build(e.clone()));

    // Phase 4 — install on Application, start background tasks
    app.imp().library_context.set(ctx.clone()).ok();
    if let Some(engine) = sync_engine { app.imp().sync_engine.set(engine).ok(); }
    app.imp().media_client.set(media_client).ok();
    app.imp().album_client.set(album_client).ok();
    app.imp().people_client.set(people_client).ok();
    app.imp().import_client.set(import_client).ok();
    if let Some(c) = sync_client { app.imp().sync_client.set(c).ok(); }

    ctx.start_purge_task();

    Ok(())
}
```

Compiler enforces phase ordering: you cannot build `MediaClientV2` before `LibraryContext` exists, because the constructor arguments don't exist.

## Decisions Recorded From Discussion

1. **No `AppContext` / `ServiceContainer` indirection layer.** The reviewer's recommendation of `Application → AppContext → ServiceContainer → Feature Services` is generic enterprise DI shaped for runtime-dynamic service sets, plugins, or multi-tenant scoping. Moments has none of those. Two layers (Application + LibraryContext) earn their keep; a third does not.

2. **`LibraryContext` is `pub(in crate::application)`, not `pub(crate)`.** `pub(crate)` would let any UI widget reach in if Application exposed an accessor — privacy would degrade to convention again. `pub(in crate::application)` makes the type literally unnameable outside the module tree.

3. **Clients hold minimal *concrete* sub-services, not the whole `LibraryContext` and not trait abstractions.** Concrete `Arc<MediaService>` over `Arc<Library>` because the constructor signature should be the explicit dependency contract; concrete over `Arc<dyn MediaService>` because there is no second implementation and no current test-mocking need (`feedback_no_preemptive_types`). Add traits later if/when polymorphism or test mocking demands it.

4. **`build` is the canonical constructor name** for clients and long-running services. Greppable, distinguishable from generic `new`, consistent across simple cases and full builder patterns.

5. **`ImportPipeline` is *not* a field on `Application`.** It is an internal implementation detail of `ImportClient`, constructed fresh per `import()` call. Only persistent stateful background services warrant a top-level Application field.

6. **`PurgeTask` is *not* a field on `Application`; its `JoinHandle` lives in `LibraryContext`.** No client invokes methods on it after startup — it is a self-contained periodic task whose only runtime input is a watch channel for cache-limit config changes. By the reachability test above, it does not need a typed identity on `Application`. If it ever grows a UI consumer (e.g. a "Purge Now" action), promote it to its own field with a paired Client at that point.

7. **`SyncEngine` is optional, gated by backend config.** `SyncClient` is `Option<SyncClient>` — matches the existing "People route hidden for Local backend" pattern.

8. **No window argument to startup or background tasks.** Toast routing already works through `crate::client::show_toast` which finds the window via `MomentsApplication::default()` and uses `glib::idle_add_once` to dispatch — safe from any thread.

## Migration Plan

Incremental, not big-bang. Each step is one PR, each compiles and passes tests on its own.

### Step 1 — Introduce `LibraryContext`, move domain fields into it

- Create `src/application/context.rs` with `LibraryContext` holding `library`, `render_pipeline`, `tokio`, `purge_handle`.
- Add `library_context: OnceCell<Arc<LibraryContext>>` to `Application` imp.
- In `load_library_async`, construct the context and populate it before populating the individual `RefCell<Option<T>>` fields.
- Keep the old `RefCell<Option<T>>` accessors as `#[deprecated]` shims that read from the context.
- Verify nothing breaks.

### Step 2 — Migrate client construction to take sub-services from `LibraryContext`

- Add `build` constructors to each client taking concrete sub-services (`Arc<MediaService>` etc.).
- Switch construction sites in startup to call `Client::build(ctx.media_service(), ...)`.
- Keep old constructors temporarily; remove once nothing else calls them.

### Step 3 — Migrate `RefCell<Option<T>>` client fields to `OnceCell<T>`

- One client at a time. For each: change field type, update accessor, remove the `.borrow().as_ref().unwrap()` chain.
- Verify each in isolation (`make lint && make test`).

### Step 4 — Remove deprecated accessors

- Drop the temporary `Application::library()`, `Application::render_pipeline()` etc. accessors.
- All UI must now go through clients; all wiring goes through `library_context()` which is `pub(in crate::application)`.

### Step 5 — Split startup into phase functions

- Extract `start()` into `src/application/startup.rs` with the four phases.
- Delete `load_library_async`.

### Step 6 — Pull `SyncEngine` out as a dedicated top-level service

- Reshape today's `SyncHandle` ownership: `SyncEngine` is the owned identity on `Application`; `SyncClient` holds a reference for UI binding.
- Conditional on backend config.

## Open Questions

1. **Where does `MutationRecorder` / `OriginalResolver` injection live?** Today they are constructed in `application/` and passed into `Library::open`. They stay there — they're construction-time arguments to `LibraryContext::build`, not fields.

2. **Test access path.** Integration tests in `tests/` cannot name `LibraryContext`. Options: (a) a `#[cfg(test)] pub` re-export from `application/`, (b) tests construct via a public `application::test_support::build_context(...)` helper, (c) tests go through full `MomentsApplication`. Likely (b) — it's a small, intentional seam.

3. **Lazy initialization.** This refactor does not introduce lazy init. The `OnceCell` shape means lazy can be added later by switching `OnceCell::get` accessors to `get_or_init`. Whether any subsystem actually benefits from lazy is a follow-up question (likely candidates: GStreamer, libheif, libraw initialization at format-decoder level — not at the client level).

4. **`AppConfig` shape.** The `immich_server_url: RefCell<Option<String>>` field on Application today suggests config is read piecemeal. An explicit `AppConfig` struct constructed once from `gio::Settings` and stored on `LibraryContext` cleans this up; the exact shape is TBD.

## Future Scopes (Deferred)

This refactor introduces exactly one typed context: `LibraryContext`. Two further scopes are anticipated and intentionally deferred. They are named here so the trajectory is visible, but no code is written for them now.

### BackgroundContext (anticipated)

A typed container for long-running background services with coordinated lifecycle operations. Would own `SyncEngine` and (when promoted from `LibraryContext`) `PurgeTask`, plus any future background workers.

**What it would do that today's per-field approach cannot:**

- `bg.pause_all()` — temporarily quiesce background tasks during heavy operations (e.g. large imports) to avoid resource contention.
- `bg.shutdown_in_order()` — drain in-flight work, then cancel tasks in a defined order before `LibraryContext` is dropped.
- `bg.apply_config(new_settings)` — push live config updates (sync interval, retention days, cache limit) to every running task that consumes them. Extends today's per-task `watch::Sender` pattern into a coordinated fan-out.

**Trigger condition for introduction:**

The *holder* shape (a struct grouping `sync_engine`, `purge_task`, etc.) becomes worthwhile once the Application field count for background work crosses ~3, purely as an organization win. The *methods* (`pause_all` etc.) are added when the first concrete coordination need arrives — designing the method set speculatively is preemptive.

**Migration cost when it arrives:** low. All fields that would move are already `pub(in crate::application)`. No widget, domain, or client changes are required — the move is purely internal to `application/`.

### ClientContext (speculative — depends on D-Bus design)

A typed container that captures the *calling environment* and configures the facade layer for it. Distinct facade types for distinct callers:

- GObject facade (today's `*Client*` types) for GTK widgets — emits property notifications, holds `ListStore` model refs.
- D-Bus facade (e.g. `DBusFacade`) for D-Bus callers — synchronous-ish variant-shaped responses, no GObject signalling.
- Possibly a CLI facade, a test facade, etc.

A `ClientContext` becomes the right abstraction *only if* cross-cutting policy needs to be configured by calling environment — rate limiting, error routing (toast vs. D-Bus signal vs. stderr), capability checks, audit logging.

**Trigger condition for introduction:**

When `docs/design-dbus-api.md` is drafted and the concrete D-Bus surface is known. The shape of `ClientContext` (and whether it's needed at all, or whether sibling facade layers without a container suffice) depends on what cross-cutting policy actually differs between callers. Designing it now would be designing a container for hypothetical contents.

**Why not pre-allocate it like `BackgroundContext`:**

The set of contents and the access pattern for `ClientContext` are not predictable without the D-Bus design. `BackgroundContext`'s contents are already known (existing background tasks). For `ClientContext` the contents *are* the design.

## What This Doesn't Solve

- **Unbounded event channels.** Separate finding in the review; needs a separate change.
- **Signal closure retention leaks.** Continuing the `glib::clone!` weak-ref sweep started in PR #662.
- **Startup performance.** Phase-based startup makes lazy initialization *possible* but doesn't itself defer anything; profiling and selective deferral is a follow-up.

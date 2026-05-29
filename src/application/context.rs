// SPDX-License-Identifier: GPL-3.0-or-later
//
// LibraryContext — owns the domain + infrastructure objects that have
// application lifetime (the `Library`, the `RenderPipeline`, the Tokio
// runtime handle, and the periodic-purge background-task `JoinHandle`).
//
// The type is `pub(in crate::application)` so it is **unnameable outside
// the `application/` module tree**. UI widgets and clients cannot import
// it — by construction, anything that needs the domain must either be
// inside `application/` or go through a UI Client. See
// `docs/design-library-context.md` for the broader design.
//
// Step 1 of the migration plan: this struct is purely additive. Existing
// `RefCell<Option<T>>` fields on `MomentsApplication` remain in place and
// continue to be populated; this context is populated *alongside* them.
// Subsequent steps will collapse the duplication.

use std::sync::{Arc, OnceLock};

use crate::library::album::AlbumService;
use crate::library::faces::FacesService;
use crate::library::thumbnail::ThumbnailService;
use crate::library::Library;
use crate::renderer::pipeline::RenderPipeline;

/// Domain + infrastructure container.
///
/// Holds the long-lived objects that drive every backend operation:
///
/// * `library` — the composed feature services.
/// * `render_pipeline` — the stateless render pipeline.
/// * `tokio` — handle to the shared Tokio runtime.
/// * `purge_handle` — `JoinHandle` for the periodic trash-purge task,
///   populated after the task is spawned at the tail of startup.
///
/// Construction is performed by [`LibraryContext::build`] from inside
/// [`crate::application::startup::phase1_domain`] once the `Library`
/// and `RenderPipeline` are ready. The context is then wrapped in an
/// `Arc` and stored in the `library_context` field on the
/// application's `imp` struct by phase 4.
///
/// `purge_handle` uses `std::sync::OnceLock` (not `std::cell::OnceCell`)
/// so the whole struct is `Send + Sync` — necessary because consumers
/// hold the context as `Arc<LibraryContext>` and the Tokio executor may
/// transport that `Arc` between threads.
pub(in crate::application) struct LibraryContext {
    library: Arc<Library>,
    render_pipeline: Arc<RenderPipeline>,
    tokio: tokio::runtime::Handle,
    purge_handle: OnceLock<tokio::task::JoinHandle<()>>,
}

impl LibraryContext {
    /// Wrap already-constructed domain + infrastructure values in a
    /// `LibraryContext`.
    ///
    /// Step 1 intentionally keeps this synchronous and minimal — it
    /// does not open the `Library` or build the `RenderPipeline`
    /// itself. Those are still constructed by
    /// [`crate::application::startup::phase1_domain`]. Later steps will
    /// move that construction into an async `build` taking a `Bundle`
    /// + config.
    pub(in crate::application) fn build(
        library: Arc<Library>,
        render_pipeline: Arc<RenderPipeline>,
        tokio: tokio::runtime::Handle,
    ) -> Arc<Self> {
        Arc::new(Self {
            library,
            render_pipeline,
            tokio,
            purge_handle: OnceLock::new(),
        })
    }

    /// The composed feature services.
    pub(in crate::application) fn library(&self) -> &Arc<Library> {
        &self.library
    }

    /// The stateless render pipeline.
    pub(in crate::application) fn render_pipeline(&self) -> &Arc<RenderPipeline> {
        &self.render_pipeline
    }

    /// A handle to the album sub-service.
    ///
    /// Returns an owned clone — `AlbumService` is a cheap `Clone` handle
    /// over the DB pool plus its event emitter. Clients that only need
    /// album operations take this instead of the whole `Arc<Library>`,
    /// so their constructor signature declares their minimal dependency
    /// surface (see `docs/design-library-context.md`).
    pub(in crate::application) fn album_service(&self) -> AlbumService {
        self.library.albums().clone()
    }

    /// A handle to the thumbnail sub-service. Owned clone; see
    /// [`LibraryContext::album_service`] for the rationale.
    pub(in crate::application) fn thumbnail_service(&self) -> ThumbnailService {
        self.library.thumbnails().clone()
    }

    /// A handle to the faces sub-service. Owned clone; see
    /// [`LibraryContext::album_service`] for the rationale.
    pub(in crate::application) fn faces_service(&self) -> FacesService {
        self.library.faces().clone()
    }

    /// Handle to the shared Tokio runtime.
    pub(in crate::application) fn tokio(&self) -> &tokio::runtime::Handle {
        &self.tokio
    }

    /// Record the `JoinHandle` returned when the periodic trash-purge
    /// task is spawned. Called exactly once during startup, after the
    /// rest of the context is wired. Returns `Err` with the unused
    /// handle if a handle was already recorded (a programmer error).
    pub(in crate::application) fn set_purge_handle(
        &self,
        handle: tokio::task::JoinHandle<()>,
    ) -> Result<(), tokio::task::JoinHandle<()>> {
        self.purge_handle.set(handle)
    }

    /// Borrow the recorded purge-task handle, if one has been set.
    ///
    /// Called from `MomentsApplication::shutdown` to abort the task
    /// before the runtime is dropped.
    pub(in crate::application) fn purge_handle(&self) -> Option<&tokio::task::JoinHandle<()>> {
        self.purge_handle.get()
    }
}

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use super::model::MediaItemObject;
use crate::library::album::{AlbumEvent, AlbumId};
use crate::library::db::LibraryStats;
use crate::library::editing::EditState;
use crate::library::error::LibraryError;
use crate::library::faces::{FacesEvent, PersonId};
use crate::library::media::{MediaCursor, MediaEvent, MediaFilter, MediaId, MediaItem};
use crate::library::metadata::MediaMetadataRecord;
use crate::library::thumbnail::ThumbnailEvent;
use crate::library::Library;

/// Number of items fetched per page. Matches `MediaClient` (v1).
const PAGE_SIZE: u32 = 250;

/// Per-model state tracked by the client. One entry per `ListStore`
/// returned from [`MediaClientV2::create_model`]. Models are tracked via
/// `WeakRef` so widgets dropping their grid also drops the tracking entry
/// on the next lookup.
struct TrackedMediaModel {
    store: glib::WeakRef<gio::ListStore>,
    filter: MediaFilter,
    cursor: RefCell<Option<MediaCursor>>,
    has_more: Cell<bool>,
    loading: Cell<bool>,
    id_index: RefCell<HashMap<MediaId, glib::WeakRef<MediaItemObject>>>,
}

/// Non-GObject dependencies set once by [`MediaClientV2::build`].
struct MediaDeps {
    library: Arc<Library>,
    tokio: tokio::runtime::Handle,
    render_pipeline: Arc<crate::renderer::pipeline::RenderPipeline>,
}

mod imp {
    use super::*;

    pub struct MediaClientV2 {
        pub(super) deps: RefCell<Option<MediaDeps>>,
        pub(super) models: RefCell<Vec<TrackedMediaModel>>,
    }

    impl Default for MediaClientV2 {
        fn default() -> Self {
            Self {
                deps: RefCell::new(None),
                models: RefCell::new(Vec::new()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaClientV2 {
        const NAME: &'static str = "MomentsMediaClientV2";
        type Type = super::MediaClientV2;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for MediaClientV2 {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // Emitted after N items are moved to trash.
                    glib::subclass::Signal::builder("items-trashed")
                        .param_types([u32::static_type()])
                        .build(),
                    // Emitted after N items are restored from trash.
                    glib::subclass::Signal::builder("items-restored")
                        .param_types([u32::static_type()])
                        .build(),
                    // Emitted after N items are permanently deleted.
                    glib::subclass::Signal::builder("items-deleted")
                        .param_types([u32::static_type()])
                        .build(),
                    // Emitted after a favourite toggle completes. (count, is_favorite).
                    glib::subclass::Signal::builder("favorite-changed")
                        .param_types([u32::static_type(), bool::static_type()])
                        .build(),
                ]
            })
        }
    }
}

glib::wrapper! {
    /// GObject singleton bridging `MediaService` to the GTK UI.
    ///
    /// Stands alongside `MediaClient` (v1) during the uplift (#587).
    /// Read-path call sites migrate to v2 incrementally; v1 retains
    /// command methods until step 3 of the uplift when EventBus is
    /// removed.
    pub struct MediaClientV2(ObjectSubclass<imp::MediaClientV2>);
}

impl MediaClientV2 {
    /// Construct an unconfigured client.
    ///
    /// Intended only for unit tests that exercise pure model-patching
    /// helpers without needing a real `Library`. Production code calls
    /// [`MediaClientV2::build`].
    #[cfg(test)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Construct a fully-wired media client.
    ///
    /// Stores dependencies and spawns the four service-event listeners
    /// on the supplied Tokio runtime:
    ///
    /// - `MediaEvent` — media-row changes (Added/Updated/Removed).
    /// - `ThumbnailEvent` — thumbnails ready on disk.
    /// - `AlbumEvent` — filtered down to `AlbumMediaChanged` for
    ///   album-filtered model refresh.
    /// - `FacesEvent` — filtered down to `PersonMediaChanged` for
    ///   person-filtered model refresh (never fires on local backends).
    pub fn build(
        library: Arc<Library>,
        tokio: tokio::runtime::Handle,
        render_pipeline: Arc<crate::renderer::pipeline::RenderPipeline>,
    ) -> Self {
        let client: Self = glib::Object::builder().build();
        let media_rx = library.media().subscribe();
        let thumb_rx = library.thumbnails().subscribe();
        let album_rx = library.albums().subscribe();
        let faces_rx = library.faces().subscribe();
        *client.imp().deps.borrow_mut() = Some(MediaDeps {
            library: Arc::clone(&library),
            tokio: tokio.clone(),
            render_pipeline,
        });
        let client_weak: glib::SendWeakRef<MediaClientV2> = client.downgrade().into();
        tokio.spawn(Self::listen_media(
            media_rx,
            Arc::clone(&library),
            client_weak.clone(),
        ));
        tokio.spawn(Self::listen_thumbnail(thumb_rx, client_weak.clone()));
        tokio.spawn(Self::listen_album(album_rx, client_weak.clone()));
        tokio.spawn(Self::listen_faces(faces_rx, client_weak));
        client
    }

    fn deps(&self) -> (Arc<Library>, tokio::runtime::Handle) {
        let deps = self.imp().deps.borrow();
        let deps = deps.as_ref().expect("MediaClientV2::build() not called");
        (Arc::clone(&deps.library), deps.tokio.clone())
    }

    /// Borrow the shared render pipeline.
    ///
    /// Exposed so widgets that already use this client (the viewer's
    /// full-res loader, the editing entry point) can decode through the
    /// same pipeline instance the client uses internally, without
    /// reaching for a global accessor on `MomentsApplication`.
    pub fn render_pipeline(&self) -> Arc<crate::renderer::pipeline::RenderPipeline> {
        let deps = self.imp().deps.borrow();
        let deps = deps.as_ref().expect("MediaClientV2::build() not called");
        Arc::clone(&deps.render_pipeline)
    }

    // ── Factory ────────────────────────────────────────────────────────

    /// Create a new paginated media list model for the given filter.
    ///
    /// The returned `ListStore` is tracked by weak reference; drop it and
    /// the tracking entry is cleaned up on the next `find_tracked` scan.
    pub fn create_model(&self, filter: MediaFilter) -> gio::ListStore {
        let store = gio::ListStore::new::<MediaItemObject>();
        let mut models = self.imp().models.borrow_mut();
        // Reclaim slots whose ListStore has been dropped by its widget.
        models.retain(|t| t.store.upgrade().is_some());
        models.push(TrackedMediaModel {
            store: store.downgrade(),
            filter,
            cursor: RefCell::new(None),
            has_more: Cell::new(true),
            loading: Cell::new(false),
            id_index: RefCell::new(HashMap::new()),
        });
        store
    }

    /// Reset pagination state and load the first page into the model.
    pub fn populate(&self, model: &gio::ListStore) {
        self.with_tracked_model(model, |tracked| {
            tracked.has_more.set(true);
            tracked.loading.set(false);
            *tracked.cursor.borrow_mut() = None;
            tracked.id_index.borrow_mut().clear();
        });
        model.remove_all();
        self.load_more(model);
    }

    /// Fetch the next page for the given model.
    ///
    /// No-op if a page is already loading or the cursor has been
    /// exhausted. Errors surface as an error toast; the loading flag is
    /// cleared so a subsequent call can retry.
    pub fn load_more(&self, model: &gio::ListStore) {
        let filter;
        let cursor;
        {
            let models = self.imp().models.borrow();
            let Some(tracked) = find_tracked(&models, model) else {
                return;
            };
            if tracked.loading.get() || !tracked.has_more.get() {
                return;
            }
            tracked.loading.set(true);
            filter = tracked.filter.clone();
            cursor = tracked.cursor.borrow().clone();
        }

        let (library, tokio) = self.deps();
        let store = model.clone();
        let client_weak: glib::SendWeakRef<MediaClientV2> = self.downgrade().into();

        debug!("loading next page (has_cursor={})", cursor.is_some());

        glib::MainContext::default().spawn_local(async move {
            let start = std::time::Instant::now();
            let result = crate::client::spawn_on(&tokio, async move {
                library
                    .media()
                    .list_media(filter, cursor.as_ref(), PAGE_SIZE)
                    .await
            })
            .await;

            let elapsed = start.elapsed();
            let Some(client) = client_weak.upgrade() else {
                return;
            };

            match result {
                Ok(items) => {
                    debug!(
                        items = items.len(),
                        elapsed_ms = elapsed.as_millis(),
                        "page fetched from database"
                    );
                    client.on_page_loaded(&store, items);
                }
                Err(e) => {
                    error!(elapsed_ms = elapsed.as_millis(), "list_media failed: {e}");
                    crate::client::show_toast("Could not load photos");
                    client.with_tracked_model(&store, |t| t.loading.set(false));
                }
            }
        });
    }

    /// Clear and reload from the first page.
    pub fn reload(&self, model: &gio::ListStore) {
        debug!("reloading grid from first page");
        self.populate(model);
    }

    /// Whether the model has more pages to load.
    pub fn has_more(&self, model: &gio::ListStore) -> bool {
        let models = self.imp().models.borrow();
        find_tracked(&models, model)
            .map(|t| t.has_more.get())
            .unwrap_or(false)
    }

    /// The filter this model was created with.
    pub fn filter_for(&self, model: &gio::ListStore) -> Option<MediaFilter> {
        let models = self.imp().models.borrow();
        find_tracked(&models, model).map(|t| t.filter.clone())
    }

    // ── Read queries ───────────────────────────────────────────────────

    /// Resolve the original file path for a media item.
    ///
    /// Async, delivered via callback on the GTK thread. For Immich
    /// libraries the resolver may fetch the file from the server first.
    pub fn original_path(&self, id: &MediaId, cb: impl FnOnce(Option<PathBuf>) + 'static) {
        let (library, tokio) = self.deps();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result =
                crate::client::spawn_on(
                    &tokio,
                    async move { library.media().original_path(&id).await },
                )
                .await;
            cb(result.ok().flatten());
        });
    }

    /// Fetch EXIF / metadata for a media item.
    pub fn media_metadata(
        &self,
        id: &MediaId,
        cb: impl FnOnce(Option<MediaMetadataRecord>) + 'static,
    ) {
        let (library, tokio) = self.deps();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = crate::client::spawn_on(&tokio, async move {
                library.metadata().media_metadata(&id).await
            })
            .await;
            cb(result.ok().flatten());
        });
    }

    /// Get the non-destructive edit state for a media item.
    pub fn get_edit_state(&self, id: &MediaId, cb: impl FnOnce(Option<EditState>) + 'static) {
        let (library, tokio) = self.deps();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = crate::client::spawn_on(&tokio, async move {
                library.editing().get_edit_state(&id).await
            })
            .await;
            cb(result.ok().flatten());
        });
    }

    /// Persist the non-destructive edit state for a media item.
    ///
    /// Phase C: when `state.has_pixel_adjustments()` returns true the
    /// edit can't be expressed as an Immich `/edits` action list, so
    /// we render a full-resolution JPEG locally and forward to
    /// [`Library::save_pixel_edit`] which embeds an XMP block,
    /// inserts a rendered media row, and enqueues the upload/stack/tag
    /// mutations. Geometric-only edits stay on the simpler Phase B path.
    pub fn save_edit_state(
        &self,
        id: &MediaId,
        state: &EditState,
        cb: impl FnOnce(Result<(), LibraryError>) + 'static,
    ) {
        let (library, tokio) = self.deps();
        let pipeline = self.render_pipeline();
        let id = id.clone();
        let state = state.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = crate::client::spawn_on(&tokio, async move {
                if state.has_pixel_adjustments() {
                    render_and_save_pixel_edit(&library, &pipeline, &id, &state).await
                } else {
                    library.editing().save_edit_state(&id, &state).await
                }
            })
            .await;
            cb(result);
        });
    }

    /// Revert edits for a media item: delete the local edit state,
    /// clean up the rendered sibling on Immich if any (Phase C §7.3),
    /// and clear any server-side `/edits` action list (Phase B path,
    /// idempotent).
    pub fn revert_edits(&self, id: &MediaId, cb: impl FnOnce(Result<(), LibraryError>) + 'static) {
        let (library, tokio) = self.deps();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result =
                crate::client::spawn_on(&tokio, async move { library.revert_edit(&id).await })
                    .await;
            cb(result);
        });
    }

    /// Fetch library statistics (total count, totals by type, etc.).
    pub fn library_stats(&self, cb: impl FnOnce(Result<LibraryStats, LibraryError>) + 'static) {
        let (library, tokio) = self.deps();

        glib::MainContext::default().spawn_local(async move {
            let result =
                crate::client::spawn_on(
                    &tokio,
                    async move { library.media().library_stats().await },
                )
                .await;
            cb(result);
        });
    }

    /// Resolve a thumbnail path. Sync — no I/O, pure path computation.
    pub fn thumbnail_path(&self, id: &MediaId) -> PathBuf {
        let (library, _) = self.deps();
        library.thumbnails().thumbnail_path(id)
    }

    // ── Commands ───────────────────────────────────────────────────────
    //
    // Each command spawns the library call on Tokio, then on success emits
    // a result signal on the GTK thread. Model reconciliation is driven by
    // the service-emitted `MediaEvent::Updated`/`Removed` events flowing
    // through `listen_media`, so the commands themselves do not patch
    // tracked models directly.

    /// Move items to trash.
    pub fn trash(&self, ids: Vec<MediaId>) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<MediaClientV2> = self.downgrade().into();
        let count = ids.len() as u32;

        glib::MainContext::default().spawn_local(async move {
            let result =
                crate::client::spawn_on(&tokio, async move { library.media().trash(&ids).await })
                    .await;

            match result {
                Ok(()) => {
                    if let Some(client) = client_weak.upgrade() {
                        client.emit_by_name::<()>("items-trashed", &[&count]);
                    }
                }
                Err(e) => {
                    error!("trash failed: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Restore items from trash.
    pub fn restore(&self, ids: Vec<MediaId>) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<MediaClientV2> = self.downgrade().into();
        let count = ids.len() as u32;

        glib::MainContext::default().spawn_local(async move {
            let result =
                crate::client::spawn_on(&tokio, async move { library.media().restore(&ids).await })
                    .await;

            match result {
                Ok(()) => {
                    if let Some(client) = client_weak.upgrade() {
                        client.emit_by_name::<()>("items-restored", &[&count]);
                    }
                }
                Err(e) => {
                    error!("restore failed: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Permanently delete items.
    ///
    /// `items-deleted` is emitted from `on_media_removed` (the
    /// `MediaEvent::Removed` listener), not directly from this method, so
    /// the signal also fires for the background purge task.
    pub fn delete(&self, ids: Vec<MediaId>) {
        let (library, tokio) = self.deps();

        glib::MainContext::default().spawn_local(async move {
            let result =
                crate::client::spawn_on(
                    &tokio,
                    async move { library.delete_permanently(&ids).await },
                )
                .await;

            if let Err(e) = result {
                error!("delete permanently failed: {e}");
                crate::client::show_error_toast(&e);
            }
        });
    }

    /// Set the favourite flag on items.
    pub fn set_favorite(&self, ids: Vec<MediaId>, state: bool) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<MediaClientV2> = self.downgrade().into();
        let count = ids.len() as u32;

        glib::MainContext::default().spawn_local(async move {
            let result = crate::client::spawn_on(&tokio, async move {
                library.media().set_favorite(&ids, state).await
            })
            .await;

            match result {
                Ok(()) => {
                    if let Some(client) = client_weak.upgrade() {
                        client.emit_by_name::<()>("favorite-changed", &[&count, &state]);
                    }
                }
                Err(e) => {
                    error!("set_favorite failed: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Permanently delete every trashed item.
    ///
    /// `items-deleted` is emitted from `on_media_removed`, not from this
    /// method.
    pub fn empty_trash(&self) {
        let (library, tokio) = self.deps();

        glib::MainContext::default().spawn_local(async move {
            let result = crate::client::spawn_on(&tokio, async move {
                let items = library
                    .media()
                    .list_media(MediaFilter::Trashed, None, u32::MAX)
                    .await?;
                let ids: Vec<_> = items.into_iter().map(|i| i.id).collect();
                if ids.is_empty() {
                    return Ok(0);
                }
                let count = ids.len();
                library.delete_permanently(&ids).await?;
                Ok(count)
            })
            .await;

            match result {
                Ok(0) => {}
                Ok(count) => info!(count, "trash emptied"),
                Err(e) => {
                    error!("empty trash failed: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Restore every trashed item.
    pub fn restore_all_trash(&self) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<MediaClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let result = crate::client::spawn_on(&tokio, async move {
                let items = library
                    .media()
                    .list_media(MediaFilter::Trashed, None, u32::MAX)
                    .await?;
                let ids: Vec<_> = items.into_iter().map(|i| i.id).collect();
                if ids.is_empty() {
                    return Ok(Vec::new());
                }
                library.media().restore(&ids).await?;
                Ok(ids)
            })
            .await;

            match result {
                Ok(ids) if ids.is_empty() => {}
                Ok(ids) => {
                    info!(count = ids.len(), "all trash restored");
                    if let Some(client) = client_weak.upgrade() {
                        client.emit_by_name::<()>("items-restored", &[&(ids.len() as u32)]);
                    }
                }
                Err(e) => {
                    error!("restore all trash failed: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    // ── Page loading helper ────────────────────────────────────────────

    fn on_page_loaded(&self, store: &gio::ListStore, items: Vec<MediaItem>) {
        let count = items.len();

        self.with_tracked_model(store, |tracked| {
            // Update cursor from last item. RecentImports sorts by
            // imported_at; everything else by taken_at (falling back to 0).
            if let Some(last) = items.last() {
                let sort_key = match tracked.filter {
                    MediaFilter::RecentImports { .. } => last.imported_at,
                    _ => last.taken_at.unwrap_or(0),
                };
                *tracked.cursor.borrow_mut() = Some(MediaCursor {
                    sort_key,
                    id: last.id.clone(),
                });
            }

            let objects: Vec<MediaItemObject> = {
                let mut index = tracked.id_index.borrow_mut();
                items
                    .into_iter()
                    .map(|item| {
                        let obj = MediaItemObject::new(item);
                        index.insert(obj.item().id.clone(), obj.downgrade());
                        obj
                    })
                    .collect()
            };

            for obj in &objects {
                store.append(obj);
            }

            if count < PAGE_SIZE as usize {
                tracked.has_more.set(false);
                debug!("all pages exhausted");
            }
            tracked.loading.set(false);
        });
    }

    fn with_tracked_model(&self, store: &gio::ListStore, f: impl FnOnce(&TrackedMediaModel)) {
        let models = self.imp().models.borrow();
        if let Some(tracked) = find_tracked(&models, store) {
            f(tracked);
        }
    }

    // ── Event listeners ────────────────────────────────────────────────

    /// Receive `MediaEvent`s from the service. Bulk-fetches the affected
    /// rows on the Tokio runtime, then dispatches reconciliation to the
    /// GTK thread via `glib::idle_add_once`.
    async fn listen_media(
        mut rx: mpsc::UnboundedReceiver<MediaEvent>,
        library: Arc<Library>,
        client_weak: glib::SendWeakRef<MediaClientV2>,
    ) {
        while let Some(event) = rx.recv().await {
            match event {
                MediaEvent::Added(ids) | MediaEvent::Updated(ids) => {
                    let items = match library.media().get_media_items(&ids).await {
                        Ok(items) => items,
                        Err(e) => {
                            error!("get_media_items failed: {e}");
                            continue;
                        }
                    };
                    let mut fetched: HashMap<MediaId, MediaItem> = HashMap::new();
                    for item in items {
                        fetched.insert(item.id.clone(), item);
                    }
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            client.on_media_changed(&ids, &fetched);
                        }
                    });
                }
                MediaEvent::Removed(ids) => {
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            client.on_media_removed(&ids);
                        }
                    });
                }
                MediaEvent::Replaced { old, new } => {
                    // Fetch the new row so we can insert it with full
                    // field data (same shape as Added).
                    let new_item = match library
                        .media()
                        .get_media_items(std::slice::from_ref(&new))
                        .await
                    {
                        Ok(mut items) => items.pop(),
                        Err(e) => {
                            error!("get_media_items failed for Replaced: {e}");
                            continue;
                        }
                    };
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            client.on_media_replaced(&old, new_item);
                        }
                    });
                }
            }
        }
        debug!("media event listener shutting down");
    }

    /// Receive `ThumbnailEvent`s and dispatch each to the GTK thread to
    /// trigger a texture load for any tracked model holding that id.
    async fn listen_thumbnail(
        mut rx: mpsc::UnboundedReceiver<ThumbnailEvent>,
        client_weak: glib::SendWeakRef<MediaClientV2>,
    ) {
        while let Some(event) = rx.recv().await {
            let ThumbnailEvent::Ready(id) = event;
            let weak = client_weak.clone();
            glib::idle_add_once(move || {
                if let Some(client) = weak.upgrade() {
                    client.on_thumbnail_ready(&id);
                }
            });
        }
        debug!("thumbnail event listener shutting down");
    }

    /// Receive `AlbumEvent`s and reload any tracked model filtered on the
    /// changed album.
    ///
    /// Only the `AlbumMediaChanged` variant triggers work — album-row
    /// add/update/remove are handled by `AlbumClientV2`. A full reload
    /// (rather than targeted reconciliation) is used because the event
    /// does not carry the affected media ids and album membership
    /// requires a DB join to determine.
    async fn listen_album(
        mut rx: mpsc::UnboundedReceiver<AlbumEvent>,
        client_weak: glib::SendWeakRef<MediaClientV2>,
    ) {
        while let Some(event) = rx.recv().await {
            match event {
                AlbumEvent::AlbumMediaChanged(album_id) => {
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            client.on_album_media_changed(&album_id);
                        }
                    });
                }
                AlbumEvent::AlbumAdded(_)
                | AlbumEvent::AlbumUpdated(_)
                | AlbumEvent::AlbumRemoved(_) => {}
            }
        }
        debug!("album event listener shutting down");
    }

    /// Receive `FacesEvent`s and reload any tracked model filtered on the
    /// changed person.
    ///
    /// Only the `PersonMediaChanged` variant triggers work — person-row
    /// add/update/remove are handled by `PeopleClientV2`. Same full-reload
    /// strategy as `listen_album`. On local backends (no face detection)
    /// this variant never fires.
    async fn listen_faces(
        mut rx: mpsc::UnboundedReceiver<FacesEvent>,
        client_weak: glib::SendWeakRef<MediaClientV2>,
    ) {
        while let Some(event) = rx.recv().await {
            match event {
                FacesEvent::PersonMediaChanged(person_id) => {
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            client.on_person_media_changed(&person_id);
                        }
                    });
                }
                FacesEvent::PersonAdded(_)
                | FacesEvent::PersonUpdated(_)
                | FacesEvent::PersonRemoved(_) => {}
            }
        }
        debug!("faces event listener shutting down");
    }

    // ── Model reconciliation (GTK thread) ──────────────────────────────

    /// Reconcile tracked models against the current DB state for the
    /// changed ids. For each supports-inline-match filter:
    ///
    /// - matches && not present  → insert sorted
    /// - matches && already there → patch scalar props in place
    /// - !matches && already there → remove from model
    /// - !matches && not present → no-op
    ///
    /// Album and Person filters are skipped — their membership requires a
    /// DB join. They are driven by `AlbumEvent::AlbumMediaChanged` and
    /// `FacesEvent::PersonMediaChanged` respectively (phase 4).
    fn on_media_changed(&self, ids: &[MediaId], fetched: &HashMap<MediaId, MediaItem>) {
        let model_snapshot: Vec<(gio::ListStore, MediaFilter)> = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .filter_map(|t| Some((t.store.upgrade()?, t.filter.clone())))
                .collect()
        };
        for (store, filter) in model_snapshot {
            if !filter.supports_inline_match() {
                continue;
            }
            for id in ids {
                let already = self.model_contains(&store, id);
                match fetched.get(id) {
                    Some(item) => {
                        if filter.matches(item) {
                            if already {
                                self.update_item_in_place(&store, item);
                            } else {
                                self.insert_item_sorted(&store, item.clone());
                            }
                        } else if already {
                            self.remove_item(&store, id);
                        }
                    }
                    None => {
                        // Row deleted between event emission and fetch.
                        if already {
                            self.remove_item(&store, id);
                        }
                    }
                }
            }
        }
    }

    fn on_media_removed(&self, ids: &[MediaId]) {
        let stores: Vec<gio::ListStore> = {
            let models = self.imp().models.borrow();
            models.iter().filter_map(|t| t.store.upgrade()).collect()
        };
        for store in stores {
            for id in ids {
                self.remove_item(&store, id);
            }
        }
        // Single source of truth for `items-deleted` — fires for user
        // deletes, `empty_trash`, AND the background purge task. Listeners
        // (e.g. the sidebar trash badge) react to all deletion paths.
        if !ids.is_empty() {
            self.emit_by_name::<()>("items-deleted", &[&(ids.len() as u32)]);
        }
    }

    /// Swap a model entry whose id was reassigned by the sync stream
    /// (local UUID → server UUID). Removes `old`, inserts `new` if the
    /// filter matches — but does **not** emit `items-deleted`. The
    /// asset wasn't deleted, only re-keyed; firing the deletion signal
    /// here would decrement the sidebar trash count and bounce users
    /// out of selection mode.
    fn on_media_replaced(&self, old: &MediaId, new_item: Option<MediaItem>) {
        let model_snapshot: Vec<(gio::ListStore, MediaFilter)> = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .filter_map(|t| Some((t.store.upgrade()?, t.filter.clone())))
                .collect()
        };
        for (store, filter) in model_snapshot {
            // Always drop the stale id from this store, regardless of
            // filter — it referenced a row that no longer exists.
            self.remove_item(&store, old);

            if !filter.supports_inline_match() {
                continue;
            }
            if let Some(item) = &new_item {
                if filter.matches(item) {
                    self.insert_item_sorted(&store, item.clone());
                }
            }
            // None new_item: server row vanished between the event
            // and the fetch. Nothing to insert. The remove above is
            // sufficient.
        }
    }

    fn on_thumbnail_ready(&self, id: &MediaId) {
        // Collect every tracked MediaItemObject for this id — the same media
        // can appear in multiple open models (e.g. All grid + album detail),
        // and each holds an independent MediaItemObject that needs the texture.
        let objs: Vec<MediaItemObject> = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .filter_map(|t| t.id_index.borrow().get(id).and_then(|w| w.upgrade()))
                .collect()
        };
        if objs.is_empty() {
            return;
        }

        let (library, tokio) = self.deps();
        let path = library.thumbnails().thumbnail_path(id);
        let id_for_log = id.clone();

        glib::MainContext::default().spawn_local(async move {
            if let Some(texture) = load_texture(tokio, path).await {
                debug!(id = %id_for_log, models = objs.len(), "thumbnail ready: texture set");
                for obj in &objs {
                    obj.set_texture(Some(texture.clone()));
                }
            }
        });
    }

    fn on_album_media_changed(&self, album_id: &AlbumId) {
        let stores: Vec<gio::ListStore> = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .filter_map(|t| {
                    if let MediaFilter::Album { album_id: ref mid } = t.filter {
                        if mid == album_id {
                            return t.store.upgrade();
                        }
                    }
                    None
                })
                .collect()
        };
        for store in stores {
            self.reload(&store);
        }
    }

    fn on_person_media_changed(&self, person_id: &PersonId) {
        let stores: Vec<gio::ListStore> = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .filter_map(|t| {
                    if let MediaFilter::Person { person_id: ref pid } = t.filter {
                        if pid == person_id {
                            return t.store.upgrade();
                        }
                    }
                    None
                })
                .collect()
        };
        for store in stores {
            self.reload(&store);
        }
    }

    // ── Model mutation helpers ─────────────────────────────────────────

    fn model_contains(&self, store: &gio::ListStore, id: &MediaId) -> bool {
        let models = self.imp().models.borrow();
        find_tracked(&models, store)
            .map(|t| t.id_index.borrow().contains_key(id))
            .unwrap_or(false)
    }

    /// Insert `item` at its sorted position in the store.
    ///
    /// Uses `taken_at` as the sort key (or `imported_at` for
    /// `RecentImports`). Ties broken by `id` for stable ordering. Binary
    /// search over `store.n_items()`.
    fn insert_item_sorted(&self, store: &gio::ListStore, item: MediaItem) {
        self.with_tracked_model(store, |tracked| {
            if tracked.id_index.borrow().contains_key(&item.id) {
                return;
            }

            let sort_key = match tracked.filter {
                MediaFilter::RecentImports { .. } => item.imported_at,
                _ => item.taken_at.unwrap_or(0),
            };

            let n = store.n_items();
            let mut lo: u32 = 0;
            let mut hi: u32 = n;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let mid_before_new = store
                    .item(mid)
                    .and_then(|o| o.downcast::<MediaItemObject>().ok())
                    .map(|obj| {
                        let obj_key = match tracked.filter {
                            MediaFilter::RecentImports { .. } => obj.item().imported_at,
                            _ => obj.item().taken_at.unwrap_or(0),
                        };
                        obj_key > sort_key
                            || (obj_key == sort_key && obj.item().id.as_str() >= item.id.as_str())
                    })
                    .unwrap_or(false);
                if mid_before_new {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }

            let obj = MediaItemObject::new(item);
            tracked
                .id_index
                .borrow_mut()
                .insert(obj.item().id.clone(), obj.downgrade());
            store.insert(lo, &obj);
        });
    }

    fn remove_item(&self, store: &gio::ListStore, id: &MediaId) {
        self.with_tracked_model(store, |tracked| {
            remove_item_from_tracked(tracked, store, id);
        });
    }

    /// Patch the mutable scalar properties of an existing
    /// `MediaItemObject` in place from a fresh DB row.
    ///
    /// The underlying `MediaItem` is stored in a `OnceCell` on the object
    /// and cannot be replaced without constructing a new object. Only
    /// `is_favorite`, `trashed_at`, and `duration_ms` are patched — fields
    /// that rarely change (taken_at, filename, etc.) would require an
    /// object replacement to take effect.
    fn update_item_in_place(&self, store: &gio::ListStore, item: &MediaItem) {
        self.with_tracked_model(store, |tracked| {
            if let Some(obj) = tracked
                .id_index
                .borrow()
                .get(&item.id)
                .and_then(|w| w.upgrade())
            {
                obj.set_is_favorite(item.is_favorite);
                obj.set_trashed_at(item.trashed_at.unwrap_or(0));
                obj.set_duration_ms(item.duration_ms.unwrap_or(0));
                // Issue #224: keep the stack badge in sync when an
                // un-stacked primary later acquires siblings, or
                // a stack is deleted server-side.
                obj.set_is_stacked(item.stack_id.is_some());
            }
        });
    }
}

/// Find the tracked entry matching a `ListStore` reference.
///
/// Entries whose `WeakRef` no longer upgrades are skipped (models whose
/// widgets have been dropped). The slot is left in place and reclaimed on
/// a future `create_model` pass.
fn find_tracked<'a>(
    models: &'a [TrackedMediaModel],
    store: &gio::ListStore,
) -> Option<&'a TrackedMediaModel> {
    models
        .iter()
        .find(|t| t.store.upgrade().map(|s| s == *store).unwrap_or(false))
}

/// Remove the item with `id` from both the `ListStore` and the
/// `id_index`. No-op if not present.
fn remove_item_from_tracked(tracked: &TrackedMediaModel, store: &gio::ListStore, id: &MediaId) {
    let pos = store.find_with_equal_func(|obj| {
        obj.downcast_ref::<MediaItemObject>()
            .map(|m| m.item().id == *id)
            .unwrap_or(false)
    });
    if let Some(pos) = pos {
        tracked.id_index.borrow_mut().remove(id);
        store.remove(pos);
    }
}

/// Phase C save path: render the original at full resolution with
/// `state` applied, JPEG-encode the result, and hand it to
/// [`crate::library::Library::save_pixel_edit`] which embeds the XMP
/// block, inserts the rendered media row, and enqueues the upload +
/// stack + tag mutations.
///
/// Runs the decode + edit pipeline on a Tokio blocking thread.
async fn render_and_save_pixel_edit(
    library: &std::sync::Arc<crate::library::Library>,
    pipeline: &std::sync::Arc<crate::renderer::pipeline::RenderPipeline>,
    id: &MediaId,
    state: &EditState,
) -> Result<(), LibraryError> {
    let original_path = library
        .media()
        .original_path(id)
        .await?
        .ok_or_else(|| LibraryError::Runtime(format!("original file for {id} not found")))?;

    let pipeline = std::sync::Arc::clone(pipeline);
    let state_for_render = state.clone();
    let jpeg = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, LibraryError> {
        let options = crate::renderer::pipeline::RenderOptions {
            size: crate::renderer::pipeline::RenderSize::FullRes,
            edits: Some(&state_for_render),
            target: crate::renderer::target::RenderTarget::Final,
        };
        let img = pipeline
            .render(&original_path, &options)
            .map_err(|e| LibraryError::Runtime(format!("render failed: {e}")))?;
        crate::renderer::output::to_jpeg(&img, 90)
            .map_err(|e| LibraryError::Runtime(format!("JPEG encode failed: {e}")))
    })
    .await
    .map_err(|e| LibraryError::Runtime(format!("render task join: {e}")))??;

    library.save_pixel_edit(id, state, jpeg).await
}

/// Load a thumbnail from disk and decode it into a `gdk::Texture`.
///
/// Runs the blocking file read and image decode on a Tokio blocking
/// thread. Returns `None` on any error (file not found, decode failure,
/// spawn error) — the caller skips the update and keeps the existing
/// texture.
///
/// Bypasses `crate::client::spawn_on` because that helper requires the
/// inner future return `Result<T, LibraryError>`; thumbnail loads are
/// best-effort and surface failure as `None`, not `LibraryError`.
async fn load_texture(handle: tokio::runtime::Handle, path: PathBuf) -> Option<gdk::Texture> {
    let result = handle
        .spawn(async move {
            tokio::task::spawn_blocking(move || -> Option<(Vec<u8>, u32, u32)> {
                let data = std::fs::read(&path).ok()?;
                let img = image::load_from_memory(&data).ok()?;
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                Some((rgba.into_raw(), w, h))
            })
            .await
            .ok()
        })
        .await
        .ok()?;
    let (pixels, width, height) = result??;
    let gbytes = glib::Bytes::from_owned(pixels);
    Some(
        gdk::MemoryTexture::new(
            width as i32,
            height as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &gbytes,
            (width as usize) * 4,
        )
        .upcast(),
    )
}

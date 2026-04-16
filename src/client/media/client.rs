use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use tracing::{debug, error};

use super::model::MediaItemObject;
use crate::app_event::AppEvent;
use crate::event_bus::{self, EventSender, Subscription};
use crate::library::db::LibraryStats;
use crate::library::editing::EditState;
use crate::library::error::LibraryError;
use crate::library::media::{MediaCursor, MediaFilter, MediaId, MediaItem};
use crate::library::metadata::MediaMetadataRecord;
use crate::library::Library;

/// Number of items fetched per page.
const PAGE_SIZE: u32 = 250;

/// Per-model state tracked by the client.
struct TrackedMediaModel {
    store: glib::WeakRef<gio::ListStore>,
    filter: MediaFilter,
    cursor: RefCell<Option<MediaCursor>>,
    has_more: Cell<bool>,
    loading: Cell<bool>,
    id_index: RefCell<HashMap<MediaId, glib::WeakRef<MediaItemObject>>>,
}

/// Non-GObject dependencies.
struct MediaDeps {
    library: Arc<Library>,
    tokio: tokio::runtime::Handle,
    bus: EventSender,
}

mod imp {
    use super::*;

    pub struct MediaClient {
        pub(super) deps: RefCell<Option<MediaDeps>>,
        pub(super) models: RefCell<Vec<TrackedMediaModel>>,
        pub(super) _subscription: RefCell<Option<Subscription>>,
        pub(super) _import_handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    impl Default for MediaClient {
        fn default() -> Self {
            Self {
                deps: RefCell::new(None),
                models: RefCell::new(Vec::new()),
                _subscription: RefCell::new(None),
                _import_handler: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaClient {
        const NAME: &'static str = "MomentsMediaClient";
        type Type = super::MediaClient;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for MediaClient {}
}

glib::wrapper! {
    /// GObject singleton that bridges media services to the GTK UI.
    ///
    /// Acts as a factory for paginated, filtered media list models.
    /// Tracks all models and patches them centrally on mutations
    /// (favorites, trash, restore, delete, sync).
    pub struct MediaClient(ObjectSubclass<imp::MediaClient>);
}

impl Default for MediaClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaClient {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Set dependencies and subscribe to events.
    pub fn configure(
        &self,
        library: Arc<Library>,
        tokio: tokio::runtime::Handle,
        bus: EventSender,
    ) {
        *self.imp().deps.borrow_mut() = Some(MediaDeps {
            library,
            tokio,
            bus,
        });

        // Subscribe to media events.
        let client_weak: glib::SendWeakRef<MediaClient> = self.downgrade().into();
        let sub = event_bus::subscribe(move |event| {
            let Some(client) = client_weak.upgrade() else {
                return;
            };
            client.handle_event(event);
        });
        *self.imp()._subscription.borrow_mut() = Some(sub);

        // Subscribe to import completion → reload all models.
        if let Some(import_client) =
            crate::application::MomentsApplication::default().import_client()
        {
            let client_weak: glib::SendWeakRef<MediaClient> = self.downgrade().into();
            let handler = import_client.connect_notify_local(Some("state"), move |client, _| {
                if client.state() == crate::client::ImportState::Complete {
                    if let Some(media_client) = client_weak.upgrade() {
                        media_client.reload_all();
                    }
                }
            });
            *self.imp()._import_handler.borrow_mut() = Some(handler);
        }
    }

    fn deps(&self) -> (Arc<Library>, tokio::runtime::Handle, EventSender) {
        let deps = self.imp().deps.borrow();
        let deps = deps.as_ref().expect("MediaClient::configure() not called");
        (deps.library.clone(), deps.tokio.clone(), deps.bus.clone())
    }

    // ── Factory ────────────────────────────────────────────────────────

    /// Create a new paginated media list model for the given filter.
    pub fn create_model(&self, filter: MediaFilter) -> gio::ListStore {
        let store = gio::ListStore::new::<MediaItemObject>();
        self.imp().models.borrow_mut().push(TrackedMediaModel {
            store: store.downgrade(),
            filter,
            cursor: RefCell::new(None),
            has_more: Cell::new(true),
            loading: Cell::new(false),
            id_index: RefCell::new(HashMap::new()),
        });
        store
    }

    /// Load the first page into the model.
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

        let (library, tokio, bus) = self.deps();
        let store = model.clone();
        let client_weak: glib::SendWeakRef<MediaClient> = self.downgrade().into();

        debug!("loading next page (has_cursor={})", cursor.is_some());

        glib::MainContext::default().spawn_local(async move {
            let start = std::time::Instant::now();
            let result = tokio
                .spawn(async move {
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
                Ok(Ok(items)) => {
                    debug!(
                        items = items.len(),
                        elapsed_ms = elapsed.as_millis(),
                        "page fetched from database"
                    );
                    client.on_page_loaded(&store, items);
                }
                Ok(Err(e)) => {
                    error!(elapsed_ms = elapsed.as_millis(), "list_media failed: {e}");
                    bus.send(AppEvent::Error("Could not load photos".into()));
                    client.with_tracked_model(&store, |t| t.loading.set(false));
                }
                Err(e) => {
                    error!(elapsed_ms = elapsed.as_millis(), "tokio join failed: {e}");
                    bus.send(AppEvent::Error("Could not load photos".into()));
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

    /// Whether the model is currently loading a page.
    pub fn is_loading(&self, model: &gio::ListStore) -> bool {
        let models = self.imp().models.borrow();
        find_tracked(&models, model)
            .map(|t| t.loading.get())
            .unwrap_or(false)
    }

    /// The filter this model was created with.
    pub fn filter_for(&self, model: &gio::ListStore) -> Option<MediaFilter> {
        let models = self.imp().models.borrow();
        find_tracked(&models, model).map(|t| t.filter.clone())
    }

    // ── Viewer queries ─────────────────────────────────────────────────

    /// Resolve the original file path for a media item.
    pub fn original_path(&self, id: &MediaId, cb: impl FnOnce(Option<PathBuf>) + 'static) {
        let (library, tokio, _) = self.deps();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.media().original_path(&id).await })
                .await;
            let path = result.ok().and_then(|r| r.ok()).flatten();
            cb(path);
        });
    }

    /// Fetch EXIF/metadata for a media item.
    pub fn media_metadata(
        &self,
        id: &MediaId,
        cb: impl FnOnce(Option<MediaMetadataRecord>) + 'static,
    ) {
        let (library, tokio, _) = self.deps();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.metadata().media_metadata(&id).await })
                .await;
            let metadata = result.ok().and_then(|r| r.ok()).flatten();
            cb(metadata);
        });
    }

    // ── Editing ────────────────────────────────────────────────────────

    /// Get the edit state for a media item.
    pub fn get_edit_state(&self, id: &MediaId, cb: impl FnOnce(Option<EditState>) + 'static) {
        let (library, tokio, _) = self.deps();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.editing().get_edit_state(&id).await })
                .await;
            let state = result.ok().and_then(|r| r.ok()).flatten();
            cb(state);
        });
    }

    /// Save the edit state for a media item.
    pub fn save_edit_state(
        &self,
        id: &MediaId,
        state: &EditState,
        cb: impl FnOnce(Result<(), LibraryError>) + 'static,
    ) {
        let (library, tokio, _) = self.deps();
        let id = id.clone();
        let state = state.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.editing().save_edit_state(&id, &state).await })
                .await;
            match result {
                Ok(r) => cb(r),
                Err(e) => cb(Err(LibraryError::Runtime(e.to_string()))),
            }
        });
    }

    /// Revert edits for a media item (delete edit state from DB).
    pub fn revert_edits(&self, id: &MediaId, cb: impl FnOnce(Result<(), LibraryError>) + 'static) {
        let (library, tokio, _) = self.deps();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.editing().revert_edits(&id).await })
                .await;
            match result {
                Ok(r) => cb(r),
                Err(e) => cb(Err(LibraryError::Runtime(e.to_string()))),
            }
        });
    }

    // ── Stats ──────────────────────────────────────────────────────────

    /// Fetch library statistics.
    pub fn library_stats(&self, cb: impl FnOnce(Result<LibraryStats, LibraryError>) + 'static) {
        let (library, tokio, _) = self.deps();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.media().library_stats().await })
                .await;
            match result {
                Ok(r) => cb(r),
                Err(e) => cb(Err(LibraryError::Runtime(e.to_string()))),
            }
        });
    }

    // ── Sync utilities ─────────────────────────────────────────────────

    /// Resolve a thumbnail path (sync, no I/O).
    pub fn thumbnail_path(&self, id: &MediaId) -> PathBuf {
        let deps = self.imp().deps.borrow();
        let deps = deps.as_ref().expect("MediaClient::configure() not called");
        deps.library.thumbnails().thumbnail_path(id)
    }

    // ── Event handling (centralized) ───────────────────────────────────

    fn handle_event(&self, event: &AppEvent) {
        match event {
            AppEvent::ThumbnailReady { media_id } => {
                self.on_thumbnail_ready(media_id);
            }
            AppEvent::FavoriteChanged { ids, is_favorite } => {
                for id in ids {
                    self.on_favorite_changed(id, *is_favorite);
                }
            }
            AppEvent::Trashed { ids } => {
                for id in ids {
                    self.on_trashed(id, true);
                }
            }
            AppEvent::Restored { ids } => {
                for id in ids {
                    self.on_trashed(id, false);
                }
            }
            AppEvent::Deleted { ids } => {
                for id in ids {
                    self.on_deleted(id);
                }
            }
            AppEvent::AssetSynced { item } => {
                self.on_asset_synced(item);
            }
            AppEvent::AssetDeletedRemote { media_id } => {
                self.on_deleted(media_id);
            }
            AppEvent::AlbumMediaChanged { album_id } => {
                // Reload any model with a matching Album filter.
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
            _ => {}
        }
    }

    fn on_thumbnail_ready(&self, id: &MediaId) {
        let obj = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .find_map(|t| t.id_index.borrow().get(id).and_then(|w| w.upgrade()))
        };

        let Some(obj) = obj else { return };

        let path = self.thumbnail_path(id);
        let (_, tokio, _) = self.deps();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            if let Some(texture) = load_texture(tokio, path).await {
                debug!(id = %id, "thumbnail ready: texture set");
                obj.set_texture(Some(texture));
            }
        });
    }

    fn on_favorite_changed(&self, id: &MediaId, is_favorite: bool) {
        let models = self.imp().models.borrow();
        for tracked in models.iter() {
            let Some(store) = tracked.store.upgrade() else {
                continue;
            };
            match tracked.filter {
                MediaFilter::All
                | MediaFilter::RecentImports { .. }
                | MediaFilter::Album { .. }
                | MediaFilter::Person { .. } => {
                    if let Some(obj) = tracked.id_index.borrow().get(id).and_then(|w| w.upgrade()) {
                        obj.set_is_favorite(is_favorite);
                    }
                }
                MediaFilter::Favorites => {
                    if is_favorite {
                        let already_present = tracked
                            .id_index
                            .borrow()
                            .get(id)
                            .and_then(|w| w.upgrade())
                            .is_some();
                        if already_present {
                            // Already in the Favorites model — nothing to do.
                        } else {
                            // Not yet in the Favorites model — fetch and insert.
                            drop(models);
                            self.fetch_and_insert_sorted(&store, id);
                            return;
                        }
                    } else {
                        remove_item_from_tracked(tracked, &store, id);
                    }
                }
                MediaFilter::Trashed => {}
            }
        }
    }

    fn on_trashed(&self, id: &MediaId, is_trashed: bool) {
        // Collect stores to modify, then drop borrow before mutating.
        let actions: Vec<(gio::ListStore, MediaFilter)> = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .filter_map(|t| {
                    let store = t.store.upgrade()?;
                    Some((store, t.filter.clone()))
                })
                .collect()
        };

        for (store, filter) in actions {
            match filter {
                MediaFilter::All
                | MediaFilter::Favorites
                | MediaFilter::RecentImports { .. }
                | MediaFilter::Album { .. }
                | MediaFilter::Person { .. } => {
                    if is_trashed {
                        self.remove_item(&store, id);
                    } else {
                        self.fetch_and_insert_sorted(&store, id);
                    }
                }
                MediaFilter::Trashed => {
                    if is_trashed {
                        self.fetch_and_insert_sorted(&store, id);
                    } else {
                        self.remove_item(&store, id);
                    }
                }
            }
        }
    }

    fn on_deleted(&self, id: &MediaId) {
        let stores: Vec<gio::ListStore> = {
            let models = self.imp().models.borrow();
            models.iter().filter_map(|t| t.store.upgrade()).collect()
        };
        for store in stores {
            self.remove_item(&store, id);
        }
    }

    // Will be revived when MediaClient v2 subscribes to MediaEvent::Added.
    #[allow(dead_code)]
    fn on_asset_imported(&self, id: &MediaId) {
        // Collect stores for filters that should show newly imported items.
        let stores: Vec<gio::ListStore> = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .filter_map(|t| {
                    match t.filter {
                        MediaFilter::All | MediaFilter::RecentImports { .. } => {}
                        _ => return None,
                    }
                    t.store.upgrade()
                })
                .collect()
        };
        for store in stores {
            self.fetch_and_insert_sorted(&store, id);
        }
    }

    fn on_asset_synced(&self, item: &MediaItem) {
        let actions: Vec<(gio::ListStore, MediaFilter, bool)> = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .filter_map(|t| {
                    let store = t.store.upgrade()?;
                    let already = t.id_index.borrow().contains_key(&item.id);
                    Some((store, t.filter.clone(), already))
                })
                .collect()
        };

        for (store, filter, already_present) in actions {
            if filter.matches(item) && !already_present {
                self.insert_item_sorted(&store, item.clone());
            } else if already_present && !filter.matches(item) && filter.supports_inline_match() {
                self.remove_item(&store, &item.id);
            }
        }
    }

    // ── Model mutation helpers (private) ────────────────────────────────

    fn on_page_loaded(&self, store: &gio::ListStore, items: Vec<MediaItem>) {
        let count = items.len();

        self.with_tracked_model(store, |tracked| {
            // Update cursor from last item.
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

            // Build objects and index.
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

    /// Fetch a single item from the DB and insert at sorted position.
    fn fetch_and_insert_sorted(&self, store: &gio::ListStore, id: &MediaId) {
        let (library, tokio, _) = self.deps();
        let store = store.clone();
        let id = id.clone();
        let client_weak: glib::SendWeakRef<MediaClient> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.media().get_media_item(&id).await })
                .await;
            let Some(client) = client_weak.upgrade() else {
                return;
            };
            match result {
                Ok(Ok(Some(item))) => client.insert_item_sorted(&store, item),
                Ok(Ok(None)) => debug!("item not found for insert"),
                Ok(Err(e)) => error!("get_media_item failed: {e}"),
                Err(e) => error!("tokio join failed: {e}"),
            }
        });
    }

    /// Reload all tracked models from their first page.
    fn reload_all(&self) {
        let stores: Vec<gio::ListStore> = {
            let models = self.imp().models.borrow();
            models.iter().filter_map(|t| t.store.upgrade()).collect()
        };
        for store in stores {
            self.reload(&store);
        }
    }

    /// Run a closure with the tracked model for the given store.
    fn with_tracked_model(&self, store: &gio::ListStore, f: impl FnOnce(&TrackedMediaModel)) {
        let models = self.imp().models.borrow();
        if let Some(tracked) = find_tracked(&models, store) {
            f(tracked);
        }
    }
}

/// Find the tracked model matching a store reference.
fn find_tracked<'a>(
    models: &'a [TrackedMediaModel],
    store: &gio::ListStore,
) -> Option<&'a TrackedMediaModel> {
    models
        .iter()
        .find(|t| t.store.upgrade().map(|s| s == *store).unwrap_or(false))
}

/// Remove an item by ID from a tracked model's store and index.
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

/// Load a thumbnail from disk and create a `gdk::Texture`.
async fn load_texture(handle: tokio::runtime::Handle, path: PathBuf) -> Option<gtk::gdk::Texture> {
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
        gtk::gdk::MemoryTexture::new(
            width as i32,
            height as i32,
            gtk::gdk::MemoryFormat::R8g8b8a8,
            &gbytes,
            (width as usize) * 4,
        )
        .upcast(),
    )
}

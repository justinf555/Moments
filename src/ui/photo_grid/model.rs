use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gtk::{gdk, gio, glib, prelude::*};
use tracing::{debug, error};

use crate::app_event::AppEvent;
use crate::event_bus::{EventBus, EventSender};
use crate::library::media::{MediaCursor, MediaFilter, MediaId, MediaItem};
use crate::library::Library;

use super::item::MediaItemObject;

/// Number of items fetched per page.
const PAGE_SIZE: u32 = 250;

/// Data and pagination state for the photo grid.
///
/// Plain struct (not GObject) — lives on the GTK main thread only, wrapped in
/// an `Rc`. The GTK layer never sees the concrete library backend: all I/O is
/// dispatched through the stored `Arc<dyn Library>` and `tokio::Handle`.
///
/// # Event flow
/// 1. App polls `mpsc::Receiver<LibraryEvent>` via `glib::idle_add_local`.
/// 2. For each `ThumbnailReady { media_id }`, app calls `on_thumbnail_ready`.
/// 3. Model spawns a GTK-local future to load the WebP bytes on the Tokio
///    blocking pool, then creates the `gdk::Texture` on the main thread and
///    sets it on the matching `MediaItemObject`.
pub struct PhotoGridModel {
    /// The backing store, shared with the `GridView` via `MultiSelection`.
    pub store: gio::ListStore,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    filter: RefCell<MediaFilter>,
    cursor: RefCell<Option<MediaCursor>>,
    loading: Cell<bool>,
    has_more: Cell<bool>,
    /// O(1) lookup: `MediaId` → weak reference to the corresponding GObject.
    id_index: RefCell<HashMap<MediaId, glib::WeakRef<MediaItemObject>>>,
    /// Called after each page loads so the scroll handler can re-check
    /// whether more pages are needed (e.g. after a fast scrollbar drag).
    on_page_ready: RefCell<Option<Box<dyn Fn()>>>,
    /// Bus sender for emitting user-facing error toasts.
    bus_sender: EventSender,
}

impl PhotoGridModel {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        filter: MediaFilter,
        bus_sender: EventSender,
    ) -> Self {
        Self {
            store: gio::ListStore::new::<MediaItemObject>(),
            library,
            tokio,
            filter: RefCell::new(filter),
            cursor: RefCell::new(None),
            loading: Cell::new(false),
            has_more: Cell::new(true),
            id_index: RefCell::new(HashMap::new()),
            on_page_ready: RefCell::new(None),
            bus_sender,
        }
    }

    /// Subscribe to relevant events on the bus.
    ///
    /// Currently handles `ThumbnailReady`. The subscription captures a weak
    /// reference to the model — when the model is dropped, the callback
    /// becomes a no-op.
    pub fn subscribe(self: &Rc<Self>, bus: &EventBus) {
        let weak = Rc::downgrade(self);
        bus.subscribe(move |event| {
            if let Some(model) = weak.upgrade() {
                model.handle_event(event);
            }
        });
    }

    /// Subscribe to the bus via the thread-local free function.
    ///
    /// Used by lazy views that don't have a direct reference to the
    /// `EventBus` struct. Safe to call from the GTK main thread after
    /// the bus has been created.
    pub fn subscribe_to_bus(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        crate::event_bus::subscribe(move |event| {
            if let Some(model) = weak.upgrade() {
                model.handle_event(event);
            }
        });
    }

    /// Dispatch a bus event to the appropriate handler.
    fn handle_event(self: &Rc<Self>, event: &AppEvent) {
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
                let filter = self.filter();
                let already_present = self.id_index.borrow().contains_key(&item.id);
                let matches = filter.matches(item);
                debug!(
                    id = %item.id,
                    filter = ?filter,
                    is_trashed = item.is_trashed,
                    matches = matches,
                    already_present = already_present,
                    store_len = self.store.n_items(),
                    "AssetSynced received"
                );
                if matches {
                    if !already_present {
                        self.insert_item_sorted(item.clone());
                    }
                } else if already_present {
                    // Item no longer matches this view's filter (e.g. it was
                    // restored on the server, so is_trashed flipped to false).
                    // Remove it so stale entries don't linger in filtered views.
                    debug!(id = %item.id, "removing synced item that no longer matches filter");
                    self.remove_item(&item.id);
                }
            }
            AppEvent::AssetDeletedRemote { media_id } => {
                self.on_deleted(media_id);
            }
            AppEvent::AlbumMediaChanged { album_id } => {
                if let MediaFilter::Album { album_id: ref mid } = self.filter() {
                    if mid == album_id {
                        self.reload();
                    }
                }
            }
            AppEvent::ImportComplete { .. } => {
                self.reload();
            }
            _ => {}
        }
    }

    /// The filter this model was constructed with.
    pub fn filter(&self) -> MediaFilter {
        self.filter.borrow().clone()
    }

    /// Register a callback invoked after each page loads.
    pub fn set_on_page_ready(&self, cb: impl Fn() + 'static) {
        *self.on_page_ready.borrow_mut() = Some(Box::new(cb));
    }

    /// Clear all items and reload from the first page.
    ///
    /// Called after an import completes so newly arrived photos appear at the
    /// top of the grid without requiring a restart.
    pub fn reload(self: &Rc<Self>) {
        self.store.remove_all();
        self.id_index.borrow_mut().clear();
        *self.cursor.borrow_mut() = None;
        self.loading.set(false);
        self.has_more.set(true);
        debug!(filter = ?self.filter(), "reloading grid from first page");
        self.load_more();
    }

    /// Fetch the next page of media items from the library.
    ///
    /// No-op if a load is already in flight or there are no more pages.
    /// Dispatches the async library call to the Tokio executor and processes
    /// the result back on the GTK main thread.
    pub fn load_more(self: &Rc<Self>) {
        if self.loading.get() || !self.has_more.get() {
            return;
        }
        self.loading.set(true);
        debug!("loading next page (has_cursor={})", self.cursor.borrow().is_some());

        let filter = self.filter.borrow().clone();
        let cursor = self.cursor.borrow().clone();
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();
        let model = Rc::clone(self);

        glib::MainContext::default().spawn_local(async move {
            let start = std::time::Instant::now();
            let result = tokio
                .spawn(async move { library.list_media(filter, cursor.as_ref(), PAGE_SIZE).await })
                .await;

            let elapsed = start.elapsed();
            match result {
                Ok(Ok(ref items)) => {
                    debug!(
                        items = items.len(),
                        elapsed_ms = elapsed.as_millis(),
                        "page fetched from database"
                    );
                    model.on_page_loaded(result.unwrap().unwrap());
                }
                Ok(Err(e)) => {
                    error!(elapsed_ms = elapsed.as_millis(), "list_media failed: {e}");
                    model.bus_sender.send(AppEvent::Error("Could not load photos".into()));
                    model.loading.set(false);
                }
                Err(e) => {
                    error!(elapsed_ms = elapsed.as_millis(), "tokio join failed: {e}");
                    model.bus_sender.send(AppEvent::Error("Could not load photos".into()));
                    model.loading.set(false);
                }
            }
        });
    }

    /// Called by the application event loop when a thumbnail arrives on disk.
    ///
    /// Finds the matching `MediaItemObject` in O(1), then spawns a future to
    /// load the WebP bytes on a Tokio blocking thread and create the
    /// `gdk::Texture` back on the GTK main thread.
    pub fn on_thumbnail_ready(self: &Rc<Self>, id: &MediaId) {
        let weak = self.id_index.borrow().get(id).cloned();
        let obj = match weak.and_then(|w| w.upgrade()) {
            Some(o) => o,
            None => return,
        };
        let path = self.library.thumbnail_path(id);
        let tokio = self.tokio.clone();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            if let Some(texture) = load_texture(tokio, path).await {
                debug!(id = %id, "thumbnail ready: texture set");
                obj.set_texture(Some(texture));
            } else {
                debug!(id = %id, "thumbnail ready: texture load failed");
            }
        });
    }

    /// Called when a favourite toggle occurs in any view.
    ///
    /// For unfiltered models (`All`): updates the `is_favorite` property on
    /// the matching `MediaItemObject` so the star icon repaints.
    ///
    /// For filtered models (`Favorites`): reloads from the database since
    /// items need to be added or removed from the filtered set.
    /// Insert a single item at the correct sorted position without clearing the store.
    ///
    /// Preserves scroll position. Skips if the item is already in the store.
    pub fn insert_item_sorted(self: &Rc<Self>, item: MediaItem) {
        // Skip duplicates.
        if self.id_index.borrow().contains_key(&item.id) {
            return;
        }

        let sort_key = match self.filter.borrow().clone() {
            MediaFilter::RecentImports { .. } => item.imported_at,
            _ => item.taken_at.unwrap_or(0),
        };

        // Find insertion position (descending order) via binary search.
        // O(log n) instead of O(n) — significant for large libraries during sync.
        let filter = self.filter.borrow().clone();
        let n = self.store.n_items();
        let mut lo: u32 = 0;
        let mut hi: u32 = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_before_new = self
                .store
                .item(mid)
                .and_then(|o| o.downcast::<MediaItemObject>().ok())
                .map(|obj| {
                    let obj_key = match filter {
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
        let pos = lo;

        let obj = MediaItemObject::new(item);
        self.id_index
            .borrow_mut()
            .insert(obj.item().id.clone(), obj.downgrade());

        // Texture loading is handled by the factory bind callback when
        // the cell becomes visible. No speculative loading here — this
        // bounds GPU memory to the visible cell count.

        self.store.insert(pos, &obj);
    }

    /// Fetch a single item from the DB and insert it at the sorted position.
    ///
    /// Used by event handlers to add items to filtered views without full reload.
    pub fn fetch_and_insert_sorted(self: &Rc<Self>, id: &MediaId) {
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();
        let model = Rc::clone(self);
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.get_media_item(&id).await })
                .await;
            match result {
                Ok(Ok(Some(item))) => model.insert_item_sorted(item),
                Ok(Ok(None)) => debug!("item not found for insert"),
                Ok(Err(e)) => error!("get_media_item failed: {e}"),
                Err(e) => error!("tokio join failed: {e}"),
            }
        });
    }

    /// Remove a single item from the store by ID.
    fn remove_item(&self, id: &MediaId) {
        let pos = self.store.find_with_equal_func(|obj| {
            obj.downcast_ref::<MediaItemObject>()
                .map(|m| m.item().id == *id)
                .unwrap_or(false)
        });
        if let Some(pos) = pos {
            self.id_index.borrow_mut().remove(id);
            self.store.remove(pos);
        }
    }

    pub fn on_favorite_changed(self: &Rc<Self>, id: &MediaId, is_favorite: bool) {
        match self.filter.borrow().clone() {
            MediaFilter::All | MediaFilter::RecentImports { .. } | MediaFilter::Album { .. } | MediaFilter::Person { .. } => {
                let weak = self.id_index.borrow().get(id).cloned();
                if let Some(obj) = weak.and_then(|w| w.upgrade()) {
                    obj.set_is_favorite(is_favorite);
                }
            }
            MediaFilter::Favorites => {
                if is_favorite {
                    self.fetch_and_insert_sorted(id);
                } else {
                    self.remove_item(id);
                }
            }
            // Trashed items can't be favourited — no action needed.
            MediaFilter::Trashed => {}
        }
    }

    /// Called when an item is trashed or restored in any view.
    pub fn on_trashed(self: &Rc<Self>, id: &MediaId, is_trashed: bool) {
        debug!(
            id = %id,
            is_trashed = is_trashed,
            filter = ?self.filter(),
            store_len = self.store.n_items(),
            "on_trashed called"
        );
        match self.filter.borrow().clone() {
            MediaFilter::All | MediaFilter::Favorites | MediaFilter::RecentImports { .. } | MediaFilter::Album { .. } | MediaFilter::Person { .. } => {
                if is_trashed {
                    // Item moved to trash — remove from this view.
                    self.remove_item(id);
                } else {
                    // Item restored — insert at sorted position.
                    self.fetch_and_insert_sorted(id);
                }
            }
            MediaFilter::Trashed => {
                if is_trashed {
                    // Item just trashed — insert into trash view.
                    self.fetch_and_insert_sorted(id);
                } else {
                    // Item restored — remove from trash view.
                    self.remove_item(id);
                }
            }
        }
    }

    /// Called when an item is permanently deleted.
    /// Removes from all views — no reload needed.
    pub fn on_deleted(self: &Rc<Self>, id: &MediaId) {
        self.remove_item(id);
    }

    fn on_page_loaded(&self, items: Vec<MediaItem>) {
        let count = items.len();
        debug!(filter = ?self.filter(), count, store_len = self.store.n_items(), "page loaded");

        // Advance the cursor to the last item so the next page continues
        // exactly where this one left off (keyset pagination).
        if let Some(last) = items.last() {
            let sort_key = match self.filter.borrow().clone() {
                MediaFilter::RecentImports { .. } => last.imported_at,
                _ => last.taken_at.unwrap_or(0),
            };
            *self.cursor.borrow_mut() = Some(MediaCursor {
                sort_key,
                id: last.id.clone(),
            });
        }

        // Build objects and index entries first, then append to the store.
        // store.append() fires items_changed synchronously, which can trigger
        // re-entrant borrows of id_index via navigate → reload. So we must
        // drop the id_index borrow before touching the store.
        let objects: Vec<MediaItemObject> = {
            let mut index = self.id_index.borrow_mut();
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
            // Texture loading is handled by the factory bind callback when
            // cells become visible. No speculative loading — bounds GPU
            // memory to the visible cell count instead of the full store.
            self.store.append(obj);
        }

        if count < PAGE_SIZE as usize {
            self.has_more.set(false);
            debug!("all pages exhausted");
        }
        self.loading.set(false);

        // Signal that a page has loaded so the scroll handler can re-evaluate
        // whether another page is needed (the user may have scrolled far ahead
        // while this page was loading).
        if let Some(cb) = self.on_page_ready.borrow().as_ref() {
            cb();
        }
    }
}

/// Load a thumbnail from disk and create a `gdk::Texture`.
///
/// Decoding runs on the Tokio blocking pool (avoids freezing the GTK thread).
/// The resulting raw RGBA pixels are handed to `gdk::MemoryTexture` on the
/// GTK main thread — no gdk-pixbuf loader required, so this works inside the
/// Flatpak sandbox where the WebP pixbuf loader is absent.
async fn load_texture(
    handle: tokio::runtime::Handle,
    path: std::path::PathBuf,
) -> Option<gdk::Texture> {
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

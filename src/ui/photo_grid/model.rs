use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib, prelude::*};
use tracing::{debug, error};

use crate::app_event::AppEvent;
use crate::event_bus::{EventBus, EventSender};
use crate::library::media::{MediaCursor, MediaFilter, MediaId, MediaItem};
use crate::library::Library;

use super::item::MediaItemObject;

/// Number of items fetched per page.
const PAGE_SIZE: u32 = 250;

// ── GObject subclass ─────────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::OnceCell;

    pub struct PhotoGridModel {
        pub store: gio::ListStore,
        pub library: OnceCell<Arc<dyn Library>>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
        pub bus_sender: OnceCell<EventSender>,
        pub filter: RefCell<MediaFilter>,
        pub cursor: RefCell<Option<MediaCursor>>,
        pub loading: Cell<bool>,
        pub has_more: Cell<bool>,
        pub id_index: RefCell<HashMap<MediaId, glib::WeakRef<MediaItemObject>>>,
        pub on_page_ready: RefCell<Option<Box<dyn Fn()>>>,
    }

    impl Default for PhotoGridModel {
        fn default() -> Self {
            Self {
                store: gio::ListStore::new::<MediaItemObject>(),
                library: OnceCell::default(),
                tokio: OnceCell::default(),
                bus_sender: OnceCell::default(),
                filter: RefCell::new(MediaFilter::All),
                cursor: RefCell::new(None),
                loading: Cell::new(false),
                has_more: Cell::new(true),
                id_index: RefCell::new(HashMap::new()),
                on_page_ready: RefCell::new(None),
            }
        }
    }

    impl PhotoGridModel {
        pub fn library(&self) -> &Arc<dyn Library> {
            self.library.get().expect("library not initialized")
        }
        pub fn tokio(&self) -> &tokio::runtime::Handle {
            self.tokio.get().expect("tokio not initialized")
        }
        pub fn bus_sender(&self) -> &EventSender {
            self.bus_sender.get().expect("bus_sender not initialized")
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PhotoGridModel {
        const NAME: &'static str = "MomentsPhotoGridModel";
        type Type = super::PhotoGridModel;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for PhotoGridModel {}
}

glib::wrapper! {
    pub struct PhotoGridModel(ObjectSubclass<imp::PhotoGridModel>);
}

impl PhotoGridModel {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        filter: MediaFilter,
        bus_sender: EventSender,
    ) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        assert!(imp.library.set(library).is_ok());
        assert!(imp.tokio.set(tokio).is_ok());
        assert!(imp.bus_sender.set(bus_sender).is_ok());
        *imp.filter.borrow_mut() = filter;
        obj
    }

    /// The backing ListStore shared with the GridView via MultiSelection.
    pub fn store(&self) -> &gio::ListStore {
        &self.imp().store
    }

    /// Subscribe to relevant events on the bus.
    pub fn subscribe(&self, bus: &EventBus) {
        let weak = self.downgrade();
        bus.subscribe(move |event| {
            if let Some(model) = weak.upgrade() {
                model.handle_event(event);
            }
        });
    }

    /// Subscribe to the bus via the thread-local free function.
    pub fn subscribe_to_bus(&self) {
        let weak = self.downgrade();
        crate::event_bus::subscribe(move |event| {
            if let Some(model) = weak.upgrade() {
                model.handle_event(event);
            }
        });
    }

    /// Dispatch a bus event to the appropriate handler.
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
                let filter = self.filter();
                let already_present = self.imp().id_index.borrow().contains_key(&item.id);
                if filter.matches(item) {
                    if !already_present {
                        self.insert_item_sorted(item.clone());
                    }
                } else if already_present && filter.supports_inline_match() {
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
        self.imp().filter.borrow().clone()
    }

    /// Register a callback invoked after each page loads.
    pub fn set_on_page_ready(&self, cb: impl Fn() + 'static) {
        *self.imp().on_page_ready.borrow_mut() = Some(Box::new(cb));
    }

    /// Clear all items and reload from the first page.
    pub fn reload(&self) {
        let imp = self.imp();
        imp.store.remove_all();
        imp.id_index.borrow_mut().clear();
        *imp.cursor.borrow_mut() = None;
        imp.loading.set(false);
        imp.has_more.set(true);
        debug!("reloading grid from first page");
        self.load_more();
    }

    /// Fetch the next page of media items from the library.
    pub fn load_more(&self) {
        let imp = self.imp();
        if imp.loading.get() || !imp.has_more.get() {
            return;
        }
        imp.loading.set(true);
        debug!(
            "loading next page (has_cursor={})",
            imp.cursor.borrow().is_some()
        );

        let filter = imp.filter.borrow().clone();
        let cursor = imp.cursor.borrow().clone();
        let library = Arc::clone(imp.library());
        let tokio = imp.tokio().clone();
        let weak = self.downgrade();

        glib::MainContext::default().spawn_local(async move {
            let start = std::time::Instant::now();
            let result = tokio
                .spawn(async move { library.list_media(filter, cursor.as_ref(), PAGE_SIZE).await })
                .await;

            let Some(model) = weak.upgrade() else { return };
            let elapsed = start.elapsed();
            match result {
                Ok(Ok(items)) => {
                    debug!(
                        items = items.len(),
                        elapsed_ms = elapsed.as_millis(),
                        "page fetched from database"
                    );
                    model.on_page_loaded(items);
                }
                Ok(Err(e)) => {
                    error!(elapsed_ms = elapsed.as_millis(), "list_media failed: {e}");
                    model
                        .imp()
                        .bus_sender()
                        .send(AppEvent::Error("Could not load photos".into()));
                    model.imp().loading.set(false);
                }
                Err(e) => {
                    error!(elapsed_ms = elapsed.as_millis(), "tokio join failed: {e}");
                    model
                        .imp()
                        .bus_sender()
                        .send(AppEvent::Error("Could not load photos".into()));
                    model.imp().loading.set(false);
                }
            }
        });
    }

    /// Called when a thumbnail arrives on disk.
    pub fn on_thumbnail_ready(&self, id: &MediaId) {
        let imp = self.imp();
        let weak = imp.id_index.borrow().get(id).cloned();
        let obj = match weak.and_then(|w| w.upgrade()) {
            Some(o) => o,
            None => return,
        };
        let path = imp.library().thumbnail_path(id);
        let tokio = imp.tokio().clone();
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

    /// Insert a single item at the correct sorted position without clearing the store.
    pub fn insert_item_sorted(&self, item: MediaItem) {
        let imp = self.imp();
        if imp.id_index.borrow().contains_key(&item.id) {
            return;
        }

        let sort_key = match imp.filter.borrow().clone() {
            MediaFilter::RecentImports { .. } => item.imported_at,
            _ => item.taken_at.unwrap_or(0),
        };

        let filter = imp.filter.borrow().clone();
        let n = imp.store.n_items();
        let mut lo: u32 = 0;
        let mut hi: u32 = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_before_new = imp
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
        imp.id_index
            .borrow_mut()
            .insert(obj.item().id.clone(), obj.downgrade());

        imp.store.insert(pos, &obj);
    }

    /// Fetch a single item from the DB and insert at sorted position.
    pub fn fetch_and_insert_sorted(&self, id: &MediaId) {
        let imp = self.imp();
        let library = Arc::clone(imp.library());
        let tokio = imp.tokio().clone();
        let weak = self.downgrade();
        let id = id.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.get_media_item(&id).await })
                .await;
            let Some(model) = weak.upgrade() else { return };
            match result {
                Ok(Ok(Some(item))) => model.insert_item_sorted(item),
                Ok(Ok(None)) => debug!("item not found for insert"),
                Ok(Err(e)) => error!("get_media_item failed: {e}"),
                Err(e) => error!("tokio join failed: {e}"),
            }
        });
    }

    fn remove_item(&self, id: &MediaId) {
        let imp = self.imp();
        let pos = imp.store.find_with_equal_func(|obj| {
            obj.downcast_ref::<MediaItemObject>()
                .map(|m| m.item().id == *id)
                .unwrap_or(false)
        });
        if let Some(pos) = pos {
            imp.id_index.borrow_mut().remove(id);
            imp.store.remove(pos);
        }
    }

    pub fn on_favorite_changed(&self, id: &MediaId, is_favorite: bool) {
        let imp = self.imp();
        match imp.filter.borrow().clone() {
            MediaFilter::All
            | MediaFilter::RecentImports { .. }
            | MediaFilter::Album { .. }
            | MediaFilter::Person { .. } => {
                let weak = imp.id_index.borrow().get(id).cloned();
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
            MediaFilter::Trashed => {}
        }
    }

    pub fn on_trashed(&self, id: &MediaId, is_trashed: bool) {
        match self.imp().filter.borrow().clone() {
            MediaFilter::All
            | MediaFilter::Favorites
            | MediaFilter::RecentImports { .. }
            | MediaFilter::Album { .. }
            | MediaFilter::Person { .. } => {
                if is_trashed {
                    self.remove_item(id);
                } else {
                    self.fetch_and_insert_sorted(id);
                }
            }
            MediaFilter::Trashed => {
                if is_trashed {
                    self.fetch_and_insert_sorted(id);
                } else {
                    self.remove_item(id);
                }
            }
        }
    }

    pub fn on_deleted(&self, id: &MediaId) {
        self.remove_item(id);
    }

    fn on_page_loaded(&self, items: Vec<MediaItem>) {
        let imp = self.imp();
        let count = items.len();
        debug!("page loaded: {count} items");

        if let Some(last) = items.last() {
            let sort_key = match imp.filter.borrow().clone() {
                MediaFilter::RecentImports { .. } => last.imported_at,
                _ => last.taken_at.unwrap_or(0),
            };
            *imp.cursor.borrow_mut() = Some(MediaCursor {
                sort_key,
                id: last.id.clone(),
            });
        }

        let objects: Vec<MediaItemObject> = {
            let mut index = imp.id_index.borrow_mut();
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
            imp.store.append(obj);
        }

        if count < PAGE_SIZE as usize {
            imp.has_more.set(false);
            debug!("all pages exhausted");
        }
        imp.loading.set(false);

        if let Some(cb) = imp.on_page_ready.borrow().as_ref() {
            cb();
        }
    }
}

/// Load a thumbnail from disk and create a `gdk::Texture`.
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

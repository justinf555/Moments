use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gtk::{gdk, gio, glib, prelude::*};
use tracing::{debug, error};

use crate::library::media::{MediaCursor, MediaFilter, MediaId, MediaItem};
use crate::library::Library;

use super::item::MediaItemObject;

/// Number of items fetched per page.
const PAGE_SIZE: u32 = 100;

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
    filter: Cell<MediaFilter>,
    cursor: RefCell<Option<MediaCursor>>,
    loading: Cell<bool>,
    has_more: Cell<bool>,
    /// O(1) lookup: `MediaId` → weak reference to the corresponding GObject.
    id_index: RefCell<HashMap<MediaId, glib::WeakRef<MediaItemObject>>>,
}

impl PhotoGridModel {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        filter: MediaFilter,
    ) -> Self {
        Self {
            store: gio::ListStore::new::<MediaItemObject>(),
            library,
            tokio,
            filter: Cell::new(filter),
            cursor: RefCell::new(None),
            loading: Cell::new(false),
            has_more: Cell::new(true),
            id_index: RefCell::new(HashMap::new()),
        }
    }

    /// The filter this model was constructed with.
    pub fn filter(&self) -> MediaFilter {
        self.filter.get()
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
        debug!("reloading grid from first page");
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

        let filter = self.filter.get();
        let cursor = self.cursor.borrow().clone();
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();
        let model = Rc::clone(self);

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.list_media(filter, cursor.as_ref(), PAGE_SIZE).await })
                .await;

            match result {
                Ok(Ok(items)) => model.on_page_loaded(items),
                Ok(Err(e)) => {
                    error!("list_media failed: {e}");
                    model.loading.set(false);
                }
                Err(e) => {
                    error!("tokio join failed: {e}");
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
        match self.filter.get() {
            MediaFilter::All | MediaFilter::RecentImports { .. } => {
                let weak = self.id_index.borrow().get(id).cloned();
                if let Some(obj) = weak.and_then(|w| w.upgrade()) {
                    obj.set_is_favorite(is_favorite);
                }
            }
            MediaFilter::Favorites => {
                if is_favorite {
                    self.reload();
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
        match self.filter.get() {
            MediaFilter::All | MediaFilter::Favorites | MediaFilter::RecentImports { .. } => {
                if is_trashed {
                    // Item moved to trash — remove from this view.
                    self.remove_item(id);
                } else {
                    // Item restored — reload to add it back in sort order.
                    self.reload();
                }
            }
            MediaFilter::Trashed => {
                if is_trashed {
                    // Item just trashed — reload to add it.
                    self.reload();
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
        debug!("page loaded: {count} items");

        // Advance the cursor to the last item so the next page continues
        // exactly where this one left off (keyset pagination).
        if let Some(last) = items.last() {
            let sort_key = match self.filter.get() {
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
            // Speculatively load any thumbnail that already exists on disk.
            let id = obj.item().id.clone();
            let path = self.library.thumbnail_path(&id);
            let tokio = self.tokio.clone();
            let obj_ref = obj.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Some(texture) = load_texture(tokio, path).await {
                    debug!(id = %id, "speculative load: texture set");
                    obj_ref.set_texture(Some(texture));
                }
            });

            self.store.append(obj);
        }

        if count < PAGE_SIZE as usize {
            self.has_more.set(false);
            debug!("all pages exhausted");
        }
        self.loading.set(false);
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

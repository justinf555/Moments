use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gtk::{gdk, gio, glib, prelude::*};
use tracing::{debug, error};

use crate::library::media::{MediaCursor, MediaId, MediaItem};
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
    cursor: RefCell<Option<MediaCursor>>,
    loading: Cell<bool>,
    has_more: Cell<bool>,
    /// O(1) lookup: `MediaId` → weak reference to the corresponding GObject.
    id_index: RefCell<HashMap<MediaId, glib::WeakRef<MediaItemObject>>>,
}

impl PhotoGridModel {
    pub fn new(library: Arc<dyn Library>, tokio: tokio::runtime::Handle) -> Self {
        Self {
            store: gio::ListStore::new::<MediaItemObject>(),
            library,
            tokio,
            cursor: RefCell::new(None),
            loading: Cell::new(false),
            has_more: Cell::new(true),
            id_index: RefCell::new(HashMap::new()),
        }
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

        let cursor = self.cursor.borrow().clone();
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();
        let model = Rc::clone(self);

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.list_media(cursor.as_ref(), PAGE_SIZE).await })
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

        glib::MainContext::default().spawn_local(async move {
            if let Some(texture) = load_texture(tokio, path).await {
                obj.set_texture(Some(texture));
            }
        });
    }

    fn on_page_loaded(&self, items: Vec<MediaItem>) {
        let count = items.len();
        debug!("page loaded: {count} items");

        // Advance the cursor to the last item so the next page continues
        // exactly where this one left off (keyset pagination).
        if let Some(last) = items.last() {
            *self.cursor.borrow_mut() = Some(MediaCursor {
                sort_key: last.taken_at.unwrap_or(0),
                id: last.id.clone(),
            });
        }

        let mut index = self.id_index.borrow_mut();
        for item in items {
            let obj = MediaItemObject::new(item);
            index.insert(obj.item().id.clone(), obj.downgrade());
            self.store.append(&obj);
        }

        if count < PAGE_SIZE as usize {
            self.has_more.set(false);
            debug!("all pages exhausted");
        }
        self.loading.set(false);
    }
}

/// Load a WebP thumbnail from disk and create a `gdk::Texture`.
///
/// File I/O runs on the Tokio blocking pool. Texture construction happens on
/// the GTK main thread (the caller's context) after the bytes arrive.
async fn load_texture(
    handle: tokio::runtime::Handle,
    path: std::path::PathBuf,
) -> Option<gdk::Texture> {
    let bytes = handle
        .spawn(async move {
            tokio::task::spawn_blocking(move || std::fs::read(&path))
                .await
                .ok()
        })
        .await
        .ok()??;
    let gbytes = glib::Bytes::from_owned(bytes.ok()?);
    gdk::Texture::from_bytes(&gbytes).ok()
}

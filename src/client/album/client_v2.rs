use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use tokio::sync::mpsc;
use tracing::{debug, error, instrument, warn};

use super::model::AlbumItemObject;
use crate::library::album::{Album, AlbumEvent, AlbumId};
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::Library;

/// Non-GObject dependencies for album operations.
struct AlbumDeps {
    library: Arc<Library>,
    tokio: tokio::runtime::Handle,
}

mod imp {
    use super::*;

    pub struct AlbumClientV2 {
        pub(super) deps: RefCell<Option<AlbumDeps>>,
        /// Weak references to all models created by this client.
        pub(super) models: RefCell<Vec<glib::WeakRef<gio::ListStore>>>,
    }

    impl Default for AlbumClientV2 {
        fn default() -> Self {
            Self {
                deps: RefCell::new(None),
                models: RefCell::new(Vec::new()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumClientV2 {
        const NAME: &'static str = "MomentsAlbumClientV2";
        type Type = super::AlbumClientV2;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for AlbumClientV2 {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // Emitted after an album is permanently deleted.
                    // Parameter: album ID string.
                    glib::subclass::Signal::builder("album-deleted")
                        .param_types([String::static_type()])
                        .build(),
                    // Emitted after an album's media list changes
                    // (add_to_album or remove_from_album completed).
                    // Parameter: album ID string.
                    glib::subclass::Signal::builder("album-media-changed")
                        .param_types([String::static_type()])
                        .build(),
                ]
            })
        }
    }
}

glib::wrapper! {
    /// GObject singleton that bridges the album service to the GTK UI.
    ///
    /// Owns command methods for album CRUD. Tracks all created ListStore
    /// models via weak references and patches them in-place on mutations.
    /// Widgets bind to the ListStore's `items-changed` signal for updates.
    ///
    /// ## Signals
    ///
    /// * `album-deleted(id: String)` — emitted after an album is deleted,
    ///   for consumers that need non-model cleanup (e.g. route unregistration).
    pub struct AlbumClientV2(ObjectSubclass<imp::AlbumClientV2>);
}

impl Default for AlbumClientV2 {
    fn default() -> Self {
        Self::new()
    }
}

impl AlbumClientV2 {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Set the dependencies required for album operations and start
    /// listening for service events.
    ///
    /// Must be called once after construction, before any other method.
    pub fn configure(
        &self,
        library: Arc<Library>,
        tokio: tokio::runtime::Handle,
        events_rx: mpsc::UnboundedReceiver<AlbumEvent>,
    ) {
        *self.imp().deps.borrow_mut() = Some(AlbumDeps {
            library: Arc::clone(&library),
            tokio: tokio.clone(),
        });

        let client_weak: glib::SendWeakRef<AlbumClientV2> = self.downgrade().into();
        tokio.spawn(Self::listen(events_rx, library, client_weak));
    }

    fn deps(&self) -> (Arc<Library>, tokio::runtime::Handle) {
        let deps = self.imp().deps.borrow();
        let deps = deps
            .as_ref()
            .expect("AlbumClientV2::configure() not called");
        (deps.library.clone(), deps.tokio.clone())
    }

    // ── Event listener ─────────────────────────────────────────────────

    /// Background task that receives `AlbumEvent`s from the service and
    /// dispatches model patches on the GTK thread.
    async fn listen(
        mut rx: mpsc::UnboundedReceiver<AlbumEvent>,
        library: Arc<Library>,
        client_weak: glib::SendWeakRef<AlbumClientV2>,
    ) {
        while let Some(event) = rx.recv().await {
            match event {
                AlbumEvent::AlbumAdded(id) => {
                    let album = library.albums().get_album(&id).await;
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            match album {
                                Ok(Some(a)) => {
                                    let album_id = a.id.clone();
                                    client.insert_into_models(a);
                                    client.load_cover_thumbnails(&album_id);
                                }
                                Ok(None) => {
                                    warn!(album_id = %id, "album not found after add event")
                                }
                                Err(e) => {
                                    error!("failed to fetch added album: {e}");
                                    crate::client::show_error_toast(&e);
                                }
                            }
                        }
                    });
                }
                AlbumEvent::AlbumUpdated(id) => {
                    let album = library.albums().get_album(&id).await;
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            match album {
                                Ok(Some(a)) => client.update_album_in_models(&a),
                                Ok(None) => {
                                    warn!(album_id = %id, "album not found after update event")
                                }
                                Err(e) => {
                                    error!("failed to fetch updated album: {e}");
                                    crate::client::show_error_toast(&e);
                                }
                            }
                        }
                    });
                }
                AlbumEvent::AlbumRemoved(id) => {
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            client.remove_from_models(&id);
                            client.emit_by_name::<()>("album-deleted", &[&id.as_str().to_string()]);
                        }
                    });
                }
            }
        }
        debug!("album event listener shutting down");
    }

    // ── Commands ──────────────────────────────────────────────────────

    /// Create a new album, optionally adding media to it.
    ///
    /// On success, inserts the album into all tracked models and loads
    /// cover thumbnails.
    #[instrument(skip(self, media_ids))]
    pub fn create_album(&self, name: String, media_ids: Vec<MediaId>) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<AlbumClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let result = crate::client::spawn_on(&tokio, async move {
                let id = library.albums().create_album(&name).await?;
                if !media_ids.is_empty() {
                    library.albums().add_to_album(&id, &media_ids).await?;
                }
                library.albums().get_album(&id).await?.ok_or_else(|| {
                    LibraryError::Runtime(format!("album {id} not found after create"))
                })
            })
            .await;

            match result {
                Ok(album) => {
                    debug!(album_id = %album.id, name = %album.name, "album created");
                    if let Some(client) = client_weak.upgrade() {
                        let album_id = album.id.clone();
                        let had_media = album.media_count > 0;
                        client.insert_into_models(album);
                        client.load_cover_thumbnails(&album_id);
                        if had_media {
                            client.emit_by_name::<()>(
                                "album-media-changed",
                                &[&album_id.as_str().to_string()],
                            );
                        }
                    }
                }
                Err(e) => {
                    error!("failed to create album: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Delete one or more albums.
    ///
    /// On success, removes from all tracked models and emits
    /// `album-deleted` for each ID.
    #[instrument(skip(self, ids))]
    pub fn delete_album(&self, ids: Vec<AlbumId>) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<AlbumClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            for id in ids {
                let lib = Arc::clone(&library);
                let aid = id.clone();
                let result = crate::client::spawn_on(&tokio, async move {
                    lib.albums().delete_album(&aid).await
                })
                .await;

                match result {
                    Ok(()) => {
                        debug!(album_id = %id, "album deleted");
                        if let Some(client) = client_weak.upgrade() {
                            client.remove_from_models(&id);
                            client.emit_by_name::<()>("album-deleted", &[&id.as_str().to_string()]);
                        }
                    }
                    Err(e) => {
                        error!(album_id = %id, "delete_album failed: {e}");
                        crate::client::show_error_toast(&e);
                    }
                }
            }
        });
    }

    /// Add media items to an album.
    ///
    /// On success, refreshes the album metadata in all tracked models.
    #[instrument(skip(self, media_ids), fields(album_id = %album_id))]
    pub fn add_to_album(&self, album_id: AlbumId, media_ids: Vec<MediaId>) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<AlbumClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let aid = album_id.clone();
            let result = crate::client::spawn_on(&tokio, async move {
                library.albums().add_to_album(&aid, &media_ids).await?;
                library
                    .albums()
                    .get_album(&aid)
                    .await?
                    .ok_or_else(|| LibraryError::Runtime(format!("album {aid} not found")))
            })
            .await;

            match result {
                Ok(album) => {
                    debug!(album_id = %album_id, "photos added to album");
                    if let Some(client) = client_weak.upgrade() {
                        client.update_album_in_models(&album);
                        tracing::debug!(album_id = %album_id, "emitting album-media-changed");
                        client.emit_by_name::<()>(
                            "album-media-changed",
                            &[&album_id.as_str().to_string()],
                        );
                    }
                }
                Err(e) => {
                    error!("add_to_album failed: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Remove media items from an album.
    ///
    /// On success, refreshes the album metadata in all tracked models.
    #[instrument(skip(self, media_ids), fields(album_id = %album_id))]
    pub fn remove_from_album(&self, album_id: AlbumId, media_ids: Vec<MediaId>) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<AlbumClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let aid = album_id.clone();
            let result = crate::client::spawn_on(&tokio, async move {
                library.albums().remove_from_album(&aid, &media_ids).await?;
                library
                    .albums()
                    .get_album(&aid)
                    .await?
                    .ok_or_else(|| LibraryError::Runtime(format!("album {aid} not found")))
            })
            .await;

            match result {
                Ok(album) => {
                    debug!(album_id = %album_id, "photos removed from album");
                    if let Some(client) = client_weak.upgrade() {
                        client.update_album_in_models(&album);
                        tracing::debug!(album_id = %album_id, "emitting album-media-changed");
                        client.emit_by_name::<()>(
                            "album-media-changed",
                            &[&album_id.as_str().to_string()],
                        );
                    }
                }
                Err(e) => {
                    error!("remove_from_album failed: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Rename an album.
    ///
    /// On success, patches the name in all tracked models.
    #[instrument(skip(self), fields(album_id = %id))]
    pub fn rename_album(&self, id: AlbumId, name: String) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<AlbumClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let rename_id = id.clone();
            let n = name.clone();
            let result = crate::client::spawn_on(&tokio, async move {
                library.albums().rename_album(&rename_id, &n).await
            })
            .await;

            match result {
                Ok(()) => {
                    debug!(album_id = %id, name = %name, "album renamed");
                    if let Some(client) = client_weak.upgrade() {
                        client.update_in_models(&id, |item| {
                            item.set_name(name.clone());
                        });
                    }
                }
                Err(e) => {
                    error!("failed to rename album: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Pin an album to the sidebar.
    pub fn pin_album(&self, id: AlbumId) {
        debug!(album_id = %id, "pin_album called");
        self.set_pinned(id, true);
    }

    /// Unpin an album from the sidebar.
    pub fn unpin_album(&self, id: AlbumId) {
        debug!(album_id = %id, "unpin_album called");
        self.set_pinned(id, false);
    }

    #[instrument(skip(self), fields(album_id = %id))]
    fn set_pinned(&self, id: AlbumId, pinned: bool) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<AlbumClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let pin_id = id.clone();
            let result = crate::client::spawn_on(&tokio, async move {
                library.albums().set_pinned(&pin_id, pinned).await
            })
            .await;

            match result {
                Ok(()) => {
                    debug!(album_id = %id, pinned, "album pin state changed");
                    if let Some(client) = client_weak.upgrade() {
                        client.update_in_models(&id, |item| {
                            item.set_pinned(pinned);
                        });
                    }
                }
                Err(e) => {
                    error!("failed to set album pinned state: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    // ── Factory ────────────────────────────────────────────────────────

    /// Create a new album list model. The client tracks it via weak ref
    /// and patches it in-place on mutations.
    pub fn create_model(&self) -> gio::ListStore {
        let store = gio::ListStore::new::<AlbumItemObject>();
        self.imp().models.borrow_mut().push(store.downgrade());
        store
    }

    /// Fetch all albums and splice into the given model.
    ///
    /// Spawns the fetch on Tokio and splices results into the store on
    /// the GTK thread. Views should call this on realize.
    #[instrument(skip(self, model))]
    pub fn list_albums(&self, model: &gio::ListStore) {
        let (library, tokio) = self.deps();
        let store = model.clone();
        let client_weak: glib::SendWeakRef<AlbumClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let result =
                crate::client::spawn_on(
                    &tokio,
                    async move { library.albums().list_albums().await },
                )
                .await;

            match result {
                Ok(albums) => {
                    let album_ids: Vec<_> = albums.iter().map(|a| a.id.clone()).collect();
                    let objects: Vec<glib::Object> = albums
                        .into_iter()
                        .map(|a| AlbumItemObject::new(a).upcast())
                        .collect();
                    store.splice(0, store.n_items(), &objects);
                    debug!(count = store.n_items(), "albums loaded");

                    // Load cover thumbnails for all albums.
                    if let Some(client) = client_weak.upgrade() {
                        for album_id in album_ids {
                            client.load_cover_thumbnails(&album_id);
                        }
                    }
                }
                Err(e) => {
                    error!("failed to load albums: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Return a snapshot of all album items from the first live model.
    ///
    /// Useful for dialogs that need a point-in-time list without creating
    /// their own model. Returns an empty vec if no models are populated.
    pub fn album_snapshot(&self) -> Vec<AlbumItemObject> {
        let models = self.imp().models.borrow();
        for weak in models.iter() {
            if let Some(store) = weak.upgrade() {
                let mut items = Vec::with_capacity(store.n_items() as usize);
                for i in 0..store.n_items() {
                    if let Some(obj) = store
                        .item(i)
                        .and_then(|o| o.downcast::<AlbumItemObject>().ok())
                    {
                        items.push(obj);
                    }
                }
                return items;
            }
        }
        Vec::new()
    }

    // ── Queries ─────────────────────────────────────────────────────────

    /// Check which albums already contain the given media items.
    ///
    /// Returns a map of album ID → count of matching media. Intended for
    /// the album picker dialog's "already added" indicator.
    ///
    /// Must be called from within a `glib::MainContext::spawn_local` block.
    #[instrument(skip(self, media_ids))]
    pub async fn album_membership(
        &self,
        media_ids: Vec<MediaId>,
    ) -> Result<HashMap<AlbumId, usize>, LibraryError> {
        let (library, tokio) = self.deps();
        crate::client::spawn_on(&tokio, async move {
            library.albums().albums_containing_media(&media_ids).await
        })
        .await
    }

    /// Patch an album's metadata in all tracked models and reload cover
    /// thumbnails.
    ///
    /// Called after mutations that change count/cover (add/remove media).
    fn update_album_in_models(&self, album: &Album) {
        let album_id = album.id.clone();
        self.update_in_models(&album.id, |item| {
            item.set_media_count(album.media_count);
            item.set_updated_at(album.updated_at);
            item.set_cover_media_id(
                album
                    .cover_media_id
                    .as_ref()
                    .map(|mid| mid.as_str().to_owned()),
            );
            for i in 0..4 {
                item.set_mosaic_texture_none(i);
            }
        });
        self.load_cover_thumbnails(&album_id);
    }

    /// Load cover thumbnails for an album and apply to the item in all models.
    #[instrument(skip(self), fields(album_id = %album_id))]
    fn load_cover_thumbnails(&self, album_id: &AlbumId) {
        let (library, tokio) = self.deps();
        let aid = album_id.clone();
        let client_weak: glib::SendWeakRef<AlbumClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            // Single spawn: fetch cover IDs + decode thumbnails.
            let aid_query = aid.clone();
            let decoded = crate::client::spawn_on(&tokio, async move {
                let cover_ids = library
                    .albums()
                    .album_cover_media_ids(&aid_query, 4)
                    .await?;
                if cover_ids.is_empty() {
                    return Ok(Vec::new());
                }

                let paths: Vec<_> = cover_ids
                    .iter()
                    .map(|mid| library.thumbnails().thumbnail_path(mid))
                    .collect();

                // File I/O + image decode are blocking — run on the
                // dedicated blocking thread pool to avoid stalling
                // Tokio async workers.
                tokio::task::spawn_blocking(move || {
                    paths
                        .into_iter()
                        .map(|path| {
                            let data = std::fs::read(&path).ok()?;
                            let img = image::load_from_memory(&data).ok()?;
                            let rgba = img.to_rgba8();
                            let (w, h) = rgba.dimensions();
                            Some((rgba.into_raw(), w, h))
                        })
                        .collect::<Vec<_>>()
                })
                .await
                .map_err(|e| LibraryError::Runtime(e.to_string()))
            })
            .await;

            let textures = match decoded {
                Ok(t) => t,
                Err(e) => {
                    error!("failed to load cover thumbnails: {e}");
                    return;
                }
            };

            let Some(client) = client_weak.upgrade() else {
                return;
            };

            client.update_in_models(&aid, |item| {
                for (i, result) in textures.iter().enumerate() {
                    if let Some((pixels, width, height)) = result {
                        item.set_mosaic_texture(i, texture_from_rgba(pixels, *width, *height));
                    }
                }
            });
        });
    }

    // ── Model patching (private) ───────────────────────────────────────

    /// Insert a new album into all tracked models.
    fn insert_into_models(&self, album: Album) {
        let models = self.imp().models.borrow();
        for weak in models.iter() {
            if let Some(store) = weak.upgrade() {
                store.append(&AlbumItemObject::new(album.clone()));
            }
        }
    }

    /// Find an album by ID across all tracked models and apply an update.
    fn update_in_models(&self, id: &AlbumId, update: impl Fn(&AlbumItemObject)) {
        let id_str = id.as_str();
        let models = self.imp().models.borrow();
        let mut updated = 0u32;
        let mut live = 0u32;
        for weak in models.iter() {
            if let Some(store) = weak.upgrade() {
                live += 1;
                if let Some((_, item)) = find_by_id(&store, id_str) {
                    update(&item);
                    updated += 1;
                }
            }
        }
        debug!(album_id = %id, live_models = live, updated_models = updated, "update_in_models");
    }

    /// Remove an album by ID from all tracked models.
    fn remove_from_models(&self, id: &AlbumId) {
        let id_str = id.as_str();
        let mut models = self.imp().models.borrow_mut();

        // Patch live models and prune dead weak refs.
        models.retain(|weak| {
            let Some(store) = weak.upgrade() else {
                return false; // Dead ref, prune.
            };
            if let Some((position, _)) = find_by_id(&store, id_str) {
                store.remove(position);
            }
            true
        });
    }
}

/// Create a `gdk::Texture` from raw RGBA pixel data.
fn texture_from_rgba(pixels: &[u8], width: u32, height: u32) -> gtk::gdk::Texture {
    let gbytes = glib::Bytes::from(pixels);
    gtk::gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &gbytes,
        (width as usize) * 4,
    )
    .upcast()
}

/// Find an `AlbumItemObject` and its position by album ID in a store.
fn find_by_id(store: &gio::ListStore, id: &str) -> Option<(u32, AlbumItemObject)> {
    for i in 0..store.n_items() {
        if let Some(obj) = store
            .item(i)
            .and_then(|o| o.downcast::<AlbumItemObject>().ok())
        {
            if obj.id() == id {
                return Some((i, obj));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_album(id: &str, name: &str) -> Album {
        Album {
            id: AlbumId::from_raw(id.to_string()),
            name: name.to_string(),
            created_at: 1000,
            updated_at: 2000,
            media_count: 0,
            cover_media_id: None,
            is_pinned: false,
        }
    }

    // ── Signal ────────────────────────────────────────────────────────

    #[test]
    fn signal_album_deleted_exists() {
        let client = AlbumClientV2::new();
        let _handler = client.connect_closure(
            "album-deleted",
            false,
            glib::closure_local!(|_client: &AlbumClientV2, _id: &str| {}),
        );
        client.emit_by_name::<()>("album-deleted", &[&"test-id".to_string()]);
    }

    // ── find_by_id ────────────────────────────────────────────────────

    #[test]
    fn find_by_id_returns_matching_item() {
        let store = gio::ListStore::new::<AlbumItemObject>();
        store.append(&AlbumItemObject::new(test_album("a1", "Alpha")));
        store.append(&AlbumItemObject::new(test_album("b2", "Beta")));

        let (pos, item) = find_by_id(&store, "b2").unwrap();
        assert_eq!(pos, 1);
        assert_eq!(item.id(), "b2");
    }

    #[test]
    fn find_by_id_returns_none_for_missing() {
        let store = gio::ListStore::new::<AlbumItemObject>();
        store.append(&AlbumItemObject::new(test_album("a1", "Alpha")));

        assert!(find_by_id(&store, "missing").is_none());
    }

    #[test]
    fn find_by_id_empty_store() {
        let store = gio::ListStore::new::<AlbumItemObject>();
        assert!(find_by_id(&store, "any").is_none());
    }

    // ── create_model ──────────────────────────────────────────────────

    #[test]
    fn create_model_tracks_weak_ref() {
        let client = AlbumClientV2::new();
        let store = client.create_model();

        assert_eq!(client.imp().models.borrow().len(), 1);
        assert!(client.imp().models.borrow()[0].upgrade().is_some());

        drop(store);
        assert!(client.imp().models.borrow()[0].upgrade().is_none());
    }

    // ── insert_into_models ────────────────────────────────────────────

    #[test]
    fn insert_into_models_adds_to_all_stores() {
        let client = AlbumClientV2::new();
        let store1 = client.create_model();
        let store2 = client.create_model();

        client.insert_into_models(test_album("a1", "Alpha"));

        assert_eq!(store1.n_items(), 1);
        assert_eq!(store2.n_items(), 1);
        let item: AlbumItemObject = store1.item(0).unwrap().downcast().unwrap();
        assert_eq!(item.name(), "Alpha");
    }

    #[test]
    fn insert_into_models_skips_dead_refs() {
        let client = AlbumClientV2::new();
        let _live = client.create_model();
        let dead = client.create_model();
        drop(dead);

        // Should not panic on the dead ref.
        client.insert_into_models(test_album("a1", "Alpha"));
        assert_eq!(_live.n_items(), 1);
    }

    // ── update_in_models ──────────────────────────────────────────────

    #[test]
    fn update_in_models_patches_all_stores() {
        let client = AlbumClientV2::new();
        let store1 = client.create_model();
        let store2 = client.create_model();

        client.insert_into_models(test_album("a1", "Old Name"));

        let id = AlbumId::from_raw("a1".to_string());
        client.update_in_models(&id, |item| {
            item.set_name("New Name".to_string());
        });

        let item1: AlbumItemObject = store1.item(0).unwrap().downcast().unwrap();
        let item2: AlbumItemObject = store2.item(0).unwrap().downcast().unwrap();
        assert_eq!(item1.name(), "New Name");
        assert_eq!(item2.name(), "New Name");
    }

    #[test]
    fn update_in_models_no_match_is_noop() {
        let client = AlbumClientV2::new();
        let store = client.create_model();
        client.insert_into_models(test_album("a1", "Alpha"));

        let id = AlbumId::from_raw("missing".to_string());
        client.update_in_models(&id, |item| {
            item.set_name("Should Not Happen".to_string());
        });

        let item: AlbumItemObject = store.item(0).unwrap().downcast().unwrap();
        assert_eq!(item.name(), "Alpha");
    }

    // ── remove_from_models ────────────────────────────────────────────

    #[test]
    fn remove_from_models_removes_from_all_stores() {
        let client = AlbumClientV2::new();
        let store1 = client.create_model();
        let store2 = client.create_model();

        client.insert_into_models(test_album("a1", "Alpha"));
        client.insert_into_models(test_album("b2", "Beta"));
        assert_eq!(store1.n_items(), 2);

        let id = AlbumId::from_raw("a1".to_string());
        client.remove_from_models(&id);

        assert_eq!(store1.n_items(), 1);
        assert_eq!(store2.n_items(), 1);
        let item: AlbumItemObject = store1.item(0).unwrap().downcast().unwrap();
        assert_eq!(item.id(), "b2");
    }

    #[test]
    fn remove_from_models_prunes_dead_refs() {
        let client = AlbumClientV2::new();
        let _live = client.create_model();
        let dead = client.create_model();
        drop(dead);

        assert_eq!(client.imp().models.borrow().len(), 2);

        let id = AlbumId::from_raw("any".to_string());
        client.remove_from_models(&id);

        // Dead ref should have been pruned.
        assert_eq!(client.imp().models.borrow().len(), 1);
    }

    // ── update_album_in_models ────────────────────────────────────────

    #[test]
    fn update_in_models_patches_metadata_fields() {
        let client = AlbumClientV2::new();
        let store = client.create_model();
        client.insert_into_models(test_album("a1", "Alpha"));

        let id = AlbumId::from_raw("a1".to_string());
        client.update_in_models(&id, |item| {
            item.set_media_count(5);
            item.set_updated_at(3000);
            item.set_cover_media_id(Some("cover123".to_string()));
            item.set_pinned(true);
        });

        let item: AlbumItemObject = store.item(0).unwrap().downcast().unwrap();
        assert_eq!(item.media_count(), 5);
        assert_eq!(item.updated_at(), 3000);
        assert_eq!(item.cover_media_id(), Some("cover123".to_string()));
        assert!(item.pinned());
    }

    // ── texture_from_rgba ─────────────────────────────────────────────

    #[test]
    fn texture_from_rgba_creates_valid_texture() {
        // 2x2 red RGBA pixels.
        let pixels = vec![
            255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        let texture = texture_from_rgba(&pixels, 2, 2);
        assert_eq!(texture.width(), 2);
        assert_eq!(texture.height(), 2);
    }
}

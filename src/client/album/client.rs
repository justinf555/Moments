use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use tracing::{debug, error};

use super::model::AlbumItemObject;
use crate::app_event::AppEvent;
use crate::event_bus::{self, EventSender, Subscription};
use crate::library::album::{Album, AlbumId};
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::Library;
use super::picker_data::{AlbumEntry, AlbumPickerData};

/// Non-GObject dependencies for album operations.
struct AlbumDeps {
    library: Arc<Library>,
    tokio: tokio::runtime::Handle,
    bus: EventSender,
}

mod imp {
    use super::*;

    pub struct AlbumClient {
        pub(super) deps: RefCell<Option<AlbumDeps>>,
        /// Weak references to all models created by this client.
        pub(super) models: RefCell<Vec<glib::WeakRef<gio::ListStore>>>,
        /// Bus subscription kept alive for the client's lifetime.
        pub(super) _subscription: RefCell<Option<Subscription>>,
    }

    impl Default for AlbumClient {
        fn default() -> Self {
            Self {
                deps: RefCell::new(None),
                models: RefCell::new(Vec::new()),
                _subscription: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumClient {
        const NAME: &'static str = "MomentsAlbumClient";
        type Type = super::AlbumClient;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for AlbumClient {}
}

glib::wrapper! {
    /// GObject singleton that bridges the album service to the GTK UI.
    ///
    /// Acts as a factory for album list models. The client tracks all created
    /// models via weak references and patches them in-place when mutations
    /// occur (create, rename, delete). Views never subscribe to bus events
    /// for album CRUD — the client handles all reactivity centrally.
    pub struct AlbumClient(ObjectSubclass<imp::AlbumClient>);
}

impl Default for AlbumClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AlbumClient {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Set the dependencies required for album operations.
    ///
    /// Must be called once after construction, before any other method.
    /// Also subscribes to `AlbumDeleted` bus events for model patching.
    pub fn configure(
        &self,
        library: Arc<Library>,
        tokio: tokio::runtime::Handle,
        bus: EventSender,
    ) {
        *self.imp().deps.borrow_mut() = Some(AlbumDeps {
            library,
            tokio,
            bus,
        });

        // Subscribe to album events from CommandDispatcher.
        let client_weak: glib::SendWeakRef<AlbumClient> = self.downgrade().into();
        let sub = event_bus::subscribe(move |event| {
            let Some(client) = client_weak.upgrade() else {
                return;
            };
            match event {
                AppEvent::AlbumCreated { ref id, ref name } => {
                    let album = crate::library::album::Album {
                        id: id.clone(),
                        name: name.clone(),
                        created_at: chrono::Utc::now().timestamp(),
                        updated_at: chrono::Utc::now().timestamp(),
                        media_count: 0,
                        cover_media_id: None,
                        is_pinned: false,
                    };
                    client.insert_into_models(album);
                }
                AppEvent::AlbumDeleted { ref id } => {
                    client.remove_from_models(id);
                }
                AppEvent::AlbumMediaChanged { ref album_id } => {
                    client.refresh_album(album_id);
                }
                _ => {}
            }
        });
        *self.imp()._subscription.borrow_mut() = Some(sub);
    }

    /// Borrow deps, clone what's needed, and return. Panics if not configured.
    fn deps(&self) -> (Arc<Library>, tokio::runtime::Handle, EventSender) {
        let deps = self.imp().deps.borrow();
        let deps = deps.as_ref().expect("AlbumClient::configure() not called");
        (deps.library.clone(), deps.tokio.clone(), deps.bus.clone())
    }

    // ── Factory ────────────────────────────────────────────────────────

    /// Create a new album list model. The client tracks it via weak ref
    /// and patches it in-place on mutations (create, rename, delete).
    pub fn create_model(&self) -> gio::ListStore {
        let store = gio::ListStore::new::<AlbumItemObject>();
        self.imp().models.borrow_mut().push(store.downgrade());
        store
    }

    /// Populate a model with all albums from the service.
    ///
    /// Spawns the fetch on Tokio and splices results into the store on the
    /// GTK thread. The view should call this on realize.
    pub fn populate(&self, model: &gio::ListStore) {
        let (library, tokio, _) = self.deps();
        let store = model.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.albums().list_albums().await })
                .await;

            match result {
                Ok(Ok(albums)) => {
                    let objects: Vec<glib::Object> = albums
                        .into_iter()
                        .map(|a| AlbumItemObject::new(a).upcast())
                        .collect();
                    store.splice(0, store.n_items(), &objects);
                    debug!(count = store.n_items(), "albums populated");
                }
                Ok(Err(e)) => error!("failed to load albums: {e}"),
                Err(e) => error!("tokio join error loading albums: {e}"),
            }
        });
    }

    // ── Mutations ──────────────────────────────────────────────────────

    /// Create a new album. On success, inserts into all tracked models
    /// and sends `AlbumCreated` on the bus.
    pub fn create_album(&self, name: String) {
        let (library, tokio, bus) = self.deps();
        let client_weak: glib::SendWeakRef<AlbumClient> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let n = name.clone();
            let result = tokio
                .spawn(async move { library.albums().create_album(&n).await })
                .await;

            match result {
                Ok(Ok(id)) => {
                    debug!(album_id = %id, name = %name, "album created");

                    // Insert into all tracked models.
                    if let Some(client) = client_weak.upgrade() {
                        let album = Album {
                            id: id.clone(),
                            name: name.clone(),
                            created_at: chrono::Utc::now().timestamp(),
                            updated_at: chrono::Utc::now().timestamp(),
                            media_count: 0,
                            cover_media_id: None,
                            is_pinned: false,
                        };
                        client.insert_into_models(album);
                    }

                    bus.send(AppEvent::AlbumCreated { id, name });
                }
                Ok(Err(e)) => {
                    error!("failed to create album: {e}");
                    bus.send(AppEvent::Error(format!("Failed to create album: {e}")));
                }
                Err(e) => {
                    error!("tokio join error: {e}");
                    bus.send(AppEvent::Error(format!("Failed to create album: {e}")));
                }
            }
        });
    }

    /// Rename an album. On success, patches the name in all tracked models
    /// and sends `AlbumRenamed` on the bus.
    pub fn rename_album(&self, id: AlbumId, name: String) {
        let (library, tokio, bus) = self.deps();
        let client_weak: glib::SendWeakRef<AlbumClient> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let rename_id = id.clone();
            let n = name.clone();
            let result = tokio
                .spawn(async move { library.albums().rename_album(&rename_id, &n).await })
                .await;

            match result {
                Ok(Ok(())) => {
                    debug!(album_id = %id, name = %name, "album renamed");

                    // Patch name in all tracked models.
                    if let Some(client) = client_weak.upgrade() {
                        client.update_in_models(&id, |item| {
                            item.set_name(name.clone());
                        });
                    }

                    bus.send(AppEvent::AlbumRenamed { id, name });
                }
                Ok(Err(e)) => {
                    error!("failed to rename album: {e}");
                    bus.send(AppEvent::Error(format!("Failed to rename album: {e}")));
                }
                Err(e) => {
                    error!("tokio join error: {e}");
                    bus.send(AppEvent::Error(format!("Failed to rename album: {e}")));
                }
            }
        });
    }

    /// Delete albums. Dispatched via the command bus (`DeleteAlbumRequested`).
    /// Model patching happens when `AlbumDeleted` arrives (see `configure`).
    pub fn delete_album(&self, ids: Vec<AlbumId>) {
        let (_, _, bus) = self.deps();
        bus.send(AppEvent::DeleteAlbumRequested { ids });
    }

    // ── Queries ─────────────────────────────────────────────────────────

    /// Fetch all albums and deliver on the GTK thread.
    ///
    /// For one-shot queries (e.g. sidebar pinned album name resolution).
    /// For views that need a live-updating model, use `create_model` +
    /// `populate` instead.
    pub fn list_albums(&self, callback: impl FnOnce(Result<Vec<Album>, LibraryError>) + 'static) {
        let (library, tokio, _) = self.deps();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.albums().list_albums().await })
                .await;

            match result {
                Ok(Ok(albums)) => callback(Ok(albums)),
                Ok(Err(e)) => {
                    error!("failed to load albums: {e}");
                    callback(Err(e));
                }
                Err(e) => {
                    error!("tokio join error loading albums: {e}");
                    callback(Err(LibraryError::Runtime(e.to_string())));
                }
            }
        });
    }

    // ── Thumbnail helpers ──────────────────────────────────────────────

    /// Fetch cover media IDs for an album and deliver on the GTK thread.
    pub fn album_cover_media_ids(
        &self,
        album_id: AlbumId,
        limit: u32,
        callback: impl FnOnce(Result<Vec<MediaId>, LibraryError>) + 'static,
    ) {
        let (library, tokio, _) = self.deps();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move {
                    library
                        .albums()
                        .album_cover_media_ids(&album_id, limit)
                        .await
                })
                .await;

            match result {
                Ok(Ok(ids)) => callback(Ok(ids)),
                Ok(Err(e)) => {
                    error!("failed to fetch album cover IDs: {e}");
                    callback(Err(e));
                }
                Err(e) => {
                    error!("tokio join error fetching cover IDs: {e}");
                    callback(Err(LibraryError::Runtime(e.to_string())));
                }
            }
        });
    }

    /// Resolve a thumbnail path for a media ID.
    ///
    /// Sync (no I/O) — just path construction.
    pub fn thumbnail_path(&self, id: &MediaId) -> PathBuf {
        let deps = self.imp().deps.borrow();
        let deps = deps.as_ref().expect("AlbumClient::configure() not called");
        deps.library.thumbnails().thumbnail_path(id)
    }

    // ── Picker dialog ──────────────────────────────────────────────────

    /// Fetch album picker data: albums + membership + decoded thumbnails.
    pub fn load_picker_data(
        &self,
        media_ids: Vec<MediaId>,
        callback: impl FnOnce(Result<AlbumPickerData, LibraryError>) + 'static,
    ) {
        let (library, tokio, _) = self.deps();

        glib::MainContext::default().spawn_local(async move {
            let svc = library.clone();
            let ids_q = media_ids.clone();
            let query_result = tokio
                .spawn(async move {
                    let albums = svc.albums().list_albums().await?;
                    let containing = svc.albums().albums_containing_media(&ids_q).await?;
                    Ok::<_, LibraryError>((albums, containing))
                })
                .await;

            let (albums, containing) = match query_result {
                Ok(Ok(pair)) => pair,
                Ok(Err(e)) => {
                    error!("album picker data load failed: {e}");
                    callback(Err(e));
                    return;
                }
                Err(e) => {
                    error!("album picker join failed: {e}");
                    callback(Err(LibraryError::Runtime(e.to_string())));
                    return;
                }
            };

            let thumb_entries: Vec<_> = albums
                .iter()
                .map(|a| {
                    let path = a
                        .cover_media_id
                        .as_ref()
                        .map(|mid| library.thumbnails().thumbnail_path(mid));
                    (a.id.clone(), path)
                })
                .collect();

            let decoded = tokio
                .spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        thumb_entries
                            .into_iter()
                            .map(|(id, path)| {
                                let rgba = path.and_then(|p| {
                                    let data = std::fs::read(&p).ok()?;
                                    let img = image::load_from_memory(&data).ok()?;
                                    let rgba = img.to_rgba8();
                                    let (w, h) = image::GenericImageView::dimensions(&rgba);
                                    Some((rgba.into_raw(), w, h))
                                });
                                (id, rgba)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await
                    .unwrap_or_default()
                })
                .await
                .unwrap_or_default();

            let decoded_map: HashMap<_, _> = decoded.into_iter().collect();

            let entries = albums
                .into_iter()
                .map(|a| {
                    let already = containing.get(&a.id).copied().unwrap_or(0);
                    let thumbnail_rgba = decoded_map.get(&a.id).and_then(|opt| opt.clone());
                    AlbumEntry {
                        id: a.id,
                        name: a.name,
                        media_count: a.media_count,
                        thumbnail_rgba,
                        already_added_count: already,
                    }
                })
                .collect();

            debug!(count = media_ids.len(), "album picker data ready");

            callback(Ok(AlbumPickerData {
                albums: entries,
                media_ids,
            }));
        });
    }

    // ── Refresh ─────────────────────────────────────────────────────────

    /// Re-fetch a single album's data from the service and patch all models.
    ///
    /// Used when album media changes (add/remove photos) to update the
    /// count, cover thumbnail, and timestamps.
    fn refresh_album(&self, album_id: &AlbumId) {
        let (library, tokio, _) = self.deps();
        let aid = album_id.clone();
        let client_weak: glib::SendWeakRef<AlbumClient> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.albums().list_albums().await })
                .await;

            let albums = match result {
                Ok(Ok(albums)) => albums,
                Ok(Err(e)) => {
                    error!("failed to refresh album: {e}");
                    return;
                }
                Err(e) => {
                    error!("tokio join error refreshing album: {e}");
                    return;
                }
            };

            let Some(album) = albums.into_iter().find(|a| a.id == aid) else {
                return;
            };

            let Some(client) = client_weak.upgrade() else {
                return;
            };

            // Patch album data in all models.
            client.update_in_models(&aid, |item| {
                item.set_media_count(album.media_count);
                item.set_updated_at(album.updated_at);
                item.set_cover_media_id(
                    album
                        .cover_media_id
                        .as_ref()
                        .map(|mid| mid.as_str().to_owned()),
                );
                // Clear stale mosaic textures before reload.
                for i in 0..4 {
                    item.set_mosaic_texture_none(i);
                }
            });

            // Reload cover thumbnails for the affected items.
            client.load_cover_thumbnails(&aid);
        });
    }

    /// Load cover thumbnails for an album and apply to the item in all models.
    fn load_cover_thumbnails(&self, album_id: &AlbumId) {
        let aid = album_id.clone();

        self.album_cover_media_ids(album_id.clone(), 4, move |result| {
            let cover_ids = match result {
                Ok(ids) => ids,
                Err(_) => return,
            };
            if cover_ids.is_empty() {
                return;
            }

            let Some(client) = crate::application::MomentsApplication::default().album_client()
            else {
                return;
            };
            let tokio = crate::application::MomentsApplication::default().tokio_handle();

            // Decode cover thumbnails on Tokio.
            let mut paths = Vec::new();
            for media_id in &cover_ids {
                paths.push(client.thumbnail_path(media_id));
            }

            let aid_inner = aid.clone();
            glib::MainContext::default().spawn_local(async move {
                let decoded = tokio
                    .spawn(async move {
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
                        .unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default();

                let Some(client) = crate::application::MomentsApplication::default().album_client()
                else {
                    return;
                };

                let id_str = aid_inner.as_str();
                let models = client.imp().models.borrow();
                for weak in models.iter() {
                    if let Some(store) = weak.upgrade() {
                        if let Some(item) = find_item_by_id(&store, id_str) {
                            for (i, result) in decoded.iter().enumerate() {
                                if let Some((pixels, width, height)) = result {
                                    let gbytes = glib::Bytes::from_owned(pixels.clone());
                                    let texture = gtk::gdk::MemoryTexture::new(
                                        *width as i32,
                                        *height as i32,
                                        gtk::gdk::MemoryFormat::R8g8b8a8,
                                        &gbytes,
                                        (*width as usize) * 4,
                                    );
                                    item.set_mosaic_texture(i, texture.upcast());
                                }
                            }
                        }
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
        for weak in models.iter() {
            if let Some(store) = weak.upgrade() {
                if let Some(item) = find_item_by_id(&store, id_str) {
                    update(&item);
                }
            }
        }
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
            if let Some(position) = find_position_by_id(&store, id_str) {
                store.remove(position);
            }
            true
        });
    }
}

/// Find an `AlbumItemObject` by album ID in a store.
fn find_item_by_id(store: &gio::ListStore, id: &str) -> Option<AlbumItemObject> {
    for i in 0..store.n_items() {
        if let Some(obj) = store
            .item(i)
            .and_then(|o| o.downcast::<AlbumItemObject>().ok())
        {
            if obj.id() == id {
                return Some(obj);
            }
        }
    }
    None
}

/// Find the position of an album by ID in a store.
fn find_position_by_id(store: &gio::ListStore, id: &str) -> Option<u32> {
    for i in 0..store.n_items() {
        if let Some(obj) = store
            .item(i)
            .and_then(|o| o.downcast::<AlbumItemObject>().ok())
        {
            if obj.id() == id {
                return Some(i);
            }
        }
    }
    None
}

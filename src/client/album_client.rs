use std::sync::Arc;

use gtk::glib;
use tracing::{debug, error};

use crate::app_event::AppEvent;
use crate::event_bus::{self, EventSender, Subscription};
use crate::library::album::AlbumId;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::Library;
use crate::ui::album_grid::item::AlbumItemObject;
use crate::ui::album_picker_dialog::{AlbumEntry, AlbumPickerData};

/// Client-side bridge between the album service and the GTK UI.
///
/// Handles the three concerns that don't belong in either layer:
/// 1. **Threading** — spawns service calls onto Tokio, delivers results on the GTK thread
/// 2. **Conversion** — transforms plain `Album` structs into `AlbumItemObject` GObjects
/// 3. **Reactivity** — subscribes to bus events and triggers UI refreshes
#[derive(Clone)]
pub struct AlbumClient {
    library: Arc<Library>,
    tokio: tokio::runtime::Handle,
    bus: EventSender,
}

impl AlbumClient {
    pub fn new(
        library: Arc<Library>,
        tokio: tokio::runtime::Handle,
        bus: EventSender,
    ) -> Self {
        Self {
            library,
            tokio,
            bus,
        }
    }

    // ── Queries (service → GObject) ─────────────────────────────────

    /// Fetch all albums and deliver as GObjects on the GTK thread.
    pub fn list_albums(
        &self,
        callback: impl FnOnce(Result<Vec<AlbumItemObject>, LibraryError>) + 'static,
    ) {
        let service = self.library.clone();
        let tokio = self.tokio.clone();

        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { service.list_albums().await })
                .await;

            match result {
                Ok(Ok(albums)) => {
                    let objects = albums.into_iter().map(AlbumItemObject::new).collect();
                    callback(Ok(objects));
                }
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

    /// Fetch album picker data: albums + membership + decoded thumbnails.
    ///
    /// This is a composite view-model query specific to the album picker
    /// dialog. It combines three service calls and thumbnail decoding into
    /// a single callback with a ready-to-present `AlbumPickerData`.
    pub fn load_picker_data(
        &self,
        media_ids: Vec<MediaId>,
        callback: impl FnOnce(Result<AlbumPickerData, LibraryError>) + 'static,
    ) {
        let library = self.library.clone();
        let tokio = self.tokio.clone();

        glib::MainContext::default().spawn_local(async move {
            // Step 1: fetch albums + membership on Tokio.
            let svc = library.clone();
            let ids_q = media_ids.clone();
            let query_result = tokio
                .spawn(async move {
                    let albums = svc.list_albums().await?;
                    let containing = svc.albums_containing_media(&ids_q).await?;
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

            // Step 2: resolve thumbnail paths (sync, on GTK thread — path
            // construction is cheap, no I/O).
            let thumb_entries: Vec<_> = albums
                .iter()
                .map(|a| {
                    let path = a
                        .cover_media_id
                        .as_ref()
                        .map(|mid| library.thumbnail_path(mid));
                    (a.id.clone(), path)
                })
                .collect();

            // Step 3: decode thumbnails on Tokio (blocking I/O).
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

            let decoded_map: std::collections::HashMap<_, _> = decoded.into_iter().collect();

            // Step 4: assemble the view-model.
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

    // ── Commands (fire-and-forget with bus notification) ─────────────

    /// Create a new album. On success, sends `AlbumCreated` on the bus.
    pub fn create_album(&self, name: String) {
        let service = self.library.clone();
        let tokio = self.tokio.clone();
        let bus = self.bus.clone();

        glib::MainContext::default().spawn_local(async move {
            let n = name.clone();
            let result = tokio
                .spawn(async move { service.create_album(&n).await })
                .await;

            match result {
                Ok(Ok(id)) => {
                    debug!(album_id = %id, name = %name, "album created");
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

    /// Rename an album. On success, sends `AlbumRenamed` on the bus.
    pub fn rename_album(&self, id: AlbumId, name: String) {
        let service = self.library.clone();
        let tokio = self.tokio.clone();
        let bus = self.bus.clone();

        glib::MainContext::default().spawn_local(async move {
            let rename_id = id.clone();
            let n = name.clone();
            let result = tokio
                .spawn(async move { service.rename_album(&rename_id, &n).await })
                .await;

            match result {
                Ok(Ok(())) => {
                    debug!(album_id = %id, name = %name, "album renamed");
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

    /// Delete an album. Dispatched via the command bus (DeleteAlbumRequested).
    ///
    /// The actual deletion is handled by the `CommandDispatcher` — this is
    /// a convenience method that sends the request event.
    pub fn delete_album(&self, ids: Vec<AlbumId>) {
        self.bus.send(AppEvent::DeleteAlbumRequested { ids });
    }

    // ── Reactivity ──────────────────────────────────────────────────

    /// Subscribe to album-related bus events. Returns a subscription handle
    /// that keeps the listener alive — drop it to unsubscribe.
    ///
    /// Calls `on_change` on the GTK thread whenever an album is created,
    /// renamed, or deleted.
    pub fn on_albums_changed(&self, on_change: impl Fn() + 'static) -> Subscription {
        event_bus::subscribe(move |event| match event {
            AppEvent::AlbumCreated { .. }
            | AppEvent::AlbumRenamed { .. }
            | AppEvent::AlbumDeleted { .. } => {
                on_change();
            }
            _ => {}
        })
    }
}

use std::rc::Rc;
use std::sync::Arc;

use gtk::{glib, prelude::*, subclass::prelude::*};
use tokio::sync::Semaphore;
use tracing::{debug, error};

use crate::library::media::MediaFilter;
use crate::library::Library;
use crate::ui::model_registry::ModelRegistry;

use super::cell::PhotoGridCell;
use super::item::MediaItemObject;
use super::texture_cache::TextureCache;

/// Concurrent thumbnail decodes: half of available cores, minimum 2.
fn max_decode_workers() -> usize {
    (std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        / 2)
    .max(2)
}

/// Build the `SignalListItemFactory` for the photo grid.
///
/// `cell_size` sets the uniform cell dimensions (px). Each cell is created
/// as a square of this size; GTK's `GridView` computes column count from the
/// available width.
///
/// `library` and `tokio` are captured by the `bind` callback so the star
/// button can persist favourite toggles without the cell needing to know
/// about the backend.
///
/// In GTK 4.12+, factory callbacks receive `&glib::Object` which may be a
/// `ListItem` or a `ListHeader`. We downcast to `gtk::ListItem` explicitly.
///
/// `setup`    — creates a fresh `PhotoGridCell` and attaches it to the list item.
/// `bind`     — connects the cell to its `MediaItemObject`, reflecting current state.
/// `unbind`   — disconnects signals and resets the cell to its idle state.
/// `teardown` — removes the child widget so GTK can reclaim the list item slot.
pub fn build_factory(
    cell_size: i32,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    registry: Rc<ModelRegistry>,
    filter: MediaFilter,
    cache: Rc<TextureCache>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let decode_semaphore = Arc::new(Semaphore::new(max_decode_workers()));

    factory.connect_setup(move |_, obj| {
        let list_item = obj
            .downcast_ref::<gtk::ListItem>()
            .expect("is ListItem");
        let cell = PhotoGridCell::new();
        cell.set_size_request(cell_size, cell_size);
        list_item.set_child(Some(&cell));
    });

    factory.connect_bind(glib::clone!(
        #[strong]
        library,
        #[strong]
        tokio,
        #[strong]
        registry,
        #[strong]
        cache,
        #[strong]
        decode_semaphore,
        move |_, obj| {
            let list_item = obj
                .downcast_ref::<gtk::ListItem>()
                .expect("is ListItem");
            let cell = list_item
                .child()
                .and_downcast::<PhotoGridCell>()
                .expect("child is PhotoGridCell");
            let item = list_item
                .item()
                .and_downcast::<MediaItemObject>()
                .expect("item is MediaItemObject");

            // Configure cell for the view type before binding.
            let is_trash = filter == MediaFilter::Trashed;
            cell.imp().show_star.set(!is_trash);

            cell.bind(&item);

            if item.texture().is_none() {
                let id = item.item().id.clone();

                // Fast path: cache hit — create GdkTexture from cached RGBA bytes.
                // No debounce needed since this is sub-millisecond.
                if let Some((pixels, width, height)) = cache.get(&id) {
                    let gbytes = glib::Bytes::from_owned(pixels);
                    let texture = gtk::gdk::MemoryTexture::new(
                        width as i32,
                        height as i32,
                        gtk::gdk::MemoryFormat::R8g8b8a8,
                        &gbytes,
                        (width as usize) * 4,
                    );
                    item.set_texture(Some(texture.upcast::<gtk::gdk::Texture>()));
                } else {
                    // Cache miss: decode immediately on the Tokio blocking pool.
                    // The semaphore limits concurrent decodes to avoid CPU contention.
                    let path = library.thumbnail_path(&id);
                    let tk = tokio.clone();
                    let item_weak = item.downgrade();
                    let cache_insert = Rc::clone(&cache);
                    let sem = Arc::clone(&decode_semaphore);

                    glib::MainContext::default().spawn_local(async move {
                        let id_for_cache = id.clone();
                        let decode_start = std::time::Instant::now();
                        let result = tk
                            .spawn(async move {
                                let _permit = sem.acquire().await.ok()?;
                                tokio::task::spawn_blocking(move || -> Option<(Vec<u8>, u32, u32)> {
                                    let data = std::fs::read(&path).ok()?;
                                    let img = image::load_from_memory(&data).ok()?;
                                    let rgba = img.to_rgba8();
                                    let (w, h) = rgba.dimensions();
                                    Some((rgba.into_raw(), w, h))
                                })
                                .await
                                .ok()?
                            })
                            .await
                            .ok();
                        if let Some(Some((pixels, width, height))) = result {
                            debug!(
                                id = %id_for_cache,
                                decode_ms = decode_start.elapsed().as_millis(),
                                "thumbnail decoded (cache miss)"
                            );
                            cache_insert.insert(id_for_cache, pixels.clone(), width, height);

                            if let Some(item) = item_weak.upgrade() {
                                let gbytes = glib::Bytes::from_owned(pixels);
                                let texture = gtk::gdk::MemoryTexture::new(
                                    width as i32,
                                    height as i32,
                                    gtk::gdk::MemoryFormat::R8g8b8a8,
                                    &gbytes,
                                    (width as usize) * 4,
                                );
                                item.set_texture(Some(texture.upcast::<gtk::gdk::Texture>()));
                            }
                        }
                    });
                }
            }

            // In Trash view: days label is shown by bind.
            // In other views: wire star button, hide days label.
            if is_trash {
                // Star already hidden via show_star flag.
            } else {
                cell.imp().days_label.set_visible(false);

                // Wire star button click → optimistic toggle + async persist.
                let star_btn = cell.imp().star_btn.clone();
                let item_weak = item.downgrade();
                let lib = Arc::clone(&library);
                let tk = tokio.clone();
                let reg = Rc::clone(&registry);
                let handler_id = star_btn.connect_clicked(move |_| {
                    let Some(item) = item_weak.upgrade() else { return };
                    let new_fav = !item.is_favorite();

                    // Optimistic: update the current item immediately.
                    item.set_is_favorite(new_fav);

                    // Persist to DB, then broadcast to all models so filtered
                    // views reload with the committed data.
                    let id = item.item().id.clone();
                    let lib = Arc::clone(&lib);
                    let tk = tk.clone();
                    let reg = Rc::clone(&reg);
                    glib::MainContext::default().spawn_local(async move {
                        let result = tk
                            .spawn(async move { lib.set_favorite(&[id.clone()], new_fav).await.map(|_| id) })
                            .await;
                        match result {
                            Ok(Ok(id)) => reg.on_favorite_changed(&id, new_fav),
                            Ok(Err(e)) => error!("set_favorite failed: {e}"),
                            Err(e) => error!("set_favorite join failed: {e}"),
                        }
                    });
                });

                cell.imp()
                    .star_click_handler
                    .borrow_mut()
                    .replace(handler_id);
            }
        }
    ));

    factory.connect_unbind(|_, obj| {
        let list_item = obj
            .downcast_ref::<gtk::ListItem>()
            .expect("is ListItem");
        let cell = list_item
            .child()
            .and_downcast::<PhotoGridCell>()
            .expect("child is PhotoGridCell");
        // Disconnect star click handler before unbinding signals.
        if let Some(handler) = cell.imp().star_click_handler.borrow_mut().take() {
            cell.imp().star_btn.disconnect(handler);
        }
        cell.unbind();

        // Release the GPU texture for off-screen items to bound VRAM usage.
        // The texture will be reloaded by the bind callback when the cell
        // becomes visible again.
        if let Some(item) = list_item
            .item()
            .and_then(|o| o.downcast::<MediaItemObject>().ok())
        {
            item.set_texture(None::<gtk::gdk::Texture>);
        }
    });

    factory.connect_teardown(|_, obj| {
        let list_item = obj
            .downcast_ref::<gtk::ListItem>()
            .expect("is ListItem");
        list_item.set_child(None::<&gtk::Widget>);
    });

    factory
}

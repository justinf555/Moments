use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gettextrs::gettext;
use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use tokio::sync::Semaphore;
use tracing::debug;

use crate::client::{MediaClient, MediaItemObject};
use crate::library::media::{MediaFilter, MediaItem};

use super::cell::PhotoGridCell;
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
#[allow(clippy::too_many_arguments)]
pub fn build_factory(
    cell_size: i32,
    media_client: MediaClient,
    filter: MediaFilter,
    cache: Rc<TextureCache>,
    selection_mode: Rc<Cell<bool>>,
    selection: gtk::MultiSelection,
    enter_selection: gio::SimpleAction,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let decode_semaphore = Arc::new(Semaphore::new(max_decode_workers()));
    let tokio = crate::application::MomentsApplication::default().tokio_handle();

    factory.connect_setup(move |_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let cell = PhotoGridCell::new();
        cell.set_size_request(cell_size, cell_size);
        list_item.set_child(Some(&cell));
    });

    factory.connect_bind(glib::clone!(
        #[strong]
        media_client,
        #[strong]
        tokio,
        #[strong]
        cache,
        #[strong]
        decode_semaphore,
        #[strong]
        selection_mode,
        #[strong]
        selection,
        #[strong]
        enter_selection,
        move |_, obj| {
            let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
            let cell = list_item
                .child()
                .and_downcast::<PhotoGridCell>()
                .expect("child is PhotoGridCell");
            let item = list_item
                .item()
                .and_downcast::<MediaItemObject>()
                .expect("item is MediaItemObject");
            let position = list_item.position();

            // Configure cell for the view type before binding.
            let is_trash = filter == MediaFilter::Trashed;
            cell.imp().show_star.set(!is_trash);

            // Set checkbox state based on current selection mode.
            cell.set_selection_mode(selection_mode.get());
            cell.set_checked(list_item.is_selected());

            cell.bind(&item);

            // Accessibility: set role and label so screen readers announce
            // "filename, date" for each cell.
            let media = item.item();
            let label = accessible_label_for_media(media);
            cell.update_property(&[gtk::accessible::Property::Label(&label)]);

            // Checkbox accessible label: "Select filename".
            let checkbox_label = format!("{} {}", gettext("Select"), media.original_filename);
            cell.imp()
                .checkbox
                .update_property(&[gtk::accessible::Property::Label(&checkbox_label)]);

            if item.texture().is_none() {
                let id = item.item().id.clone();

                // Fast path: cache hit — create GdkTexture from cached RGBA bytes.
                if let Some((gbytes, width, height)) = cache.get(&id) {
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
                    let path = media_client.thumbnail_path(&id);
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
                                tokio::task::spawn_blocking(
                                    move || -> Option<(Vec<u8>, u32, u32)> {
                                        let data = std::fs::read(&path).ok()?;
                                        let img = image::load_from_memory(&data).ok()?;
                                        let rgba = img.to_rgba8();
                                        let (w, h) = rgba.dimensions();
                                        Some((rgba.into_raw(), w, h))
                                    },
                                )
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
                            let gbytes = cache_insert.insert(id_for_cache, pixels, width, height);

                            if let Some(item) = item_weak.upgrade() {
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

                // Wire star button click → optimistic toggle + MediaClient command.
                let star_btn = cell.imp().star_btn.clone();
                let item_weak = item.downgrade();
                let mc = media_client.clone();
                let handler_id = star_btn.connect_clicked(move |_| {
                    let Some(item) = item_weak.upgrade() else {
                        return;
                    };
                    let new_fav = !item.is_favorite();
                    item.set_is_favorite(new_fav);
                    let id = item.item().id.clone();
                    mc.set_favorite(vec![id], new_fav);
                });

                cell.imp()
                    .star_click_handler
                    .borrow_mut()
                    .replace(handler_id);
            }

            // Wire checkbox → select/deselect item + enter selection mode.
            {
                let checkbox = cell.imp().checkbox.clone();
                let sel = selection.clone();
                let enter = enter_selection.clone();
                let sm = Rc::clone(&selection_mode);
                let handler_id = checkbox.connect_toggled(move |cb| {
                    if cb.is_active() {
                        if !sm.get() {
                            enter.activate(None);
                        }
                        sel.select_item(position, false);
                    } else {
                        sel.unselect_item(position);
                    }
                });
                cell.imp().checkbox_handler.borrow_mut().replace(handler_id);
            }
        }
    ));

    factory.connect_unbind(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let cell = list_item
            .child()
            .and_downcast::<PhotoGridCell>()
            .expect("child is PhotoGridCell");
        if let Some(handler) = cell.imp().star_click_handler.borrow_mut().take() {
            cell.imp().star_btn.disconnect(handler);
        }
        if let Some(handler) = cell.imp().checkbox_handler.borrow_mut().take() {
            cell.imp().checkbox.disconnect(handler);
        }
        cell.unbind();

        if let Some(item) = list_item
            .item()
            .and_then(|o| o.downcast::<MediaItemObject>().ok())
        {
            item.set_texture(None::<gtk::gdk::Texture>);
        }
    });

    factory.connect_teardown(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        list_item.set_child(None::<&gtk::Widget>);
    });

    factory
}

/// Build a human-readable accessible label from a media item's filename and
/// capture date.
fn accessible_label_for_media(item: &MediaItem) -> String {
    if let Some(ts) = item.taken_at {
        let date_str = glib::DateTime::from_unix_utc(ts)
            .ok()
            .and_then(|d| d.to_local().ok())
            .and_then(|d| d.format("%e %B %Y").ok())
            .map(|s| s.to_string());
        match date_str {
            Some(dt) if !dt.is_empty() => format!("{}, {}", item.original_filename, dt.trim()),
            _ => item.original_filename.clone(),
        }
    } else {
        item.original_filename.clone()
    }
}

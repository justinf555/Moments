use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gettextrs::gettext;
use gtk::{prelude::*, subclass::prelude::*};
use tracing::warn;

use crate::library::album::AlbumId;
use crate::library::Library;

use super::card::AlbumCard;
use super::item::AlbumItemObject;

/// Build a `SignalListItemFactory` for the album grid.
pub fn build_factory(
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    selection_mode: Rc<Cell<bool>>,
    selection: gtk::MultiSelection,
    enter_selection: gtk::gio::SimpleAction,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let card = AlbumCard::new();
        card.set_size_request(205, 205 + 52); // Cover + labels.
        list_item.set_child(Some(&card));
    });

    factory.connect_bind(move |_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let card = list_item
            .child()
            .and_downcast::<AlbumCard>()
            .expect("child is AlbumCard");
        let item = list_item
            .item()
            .and_downcast::<AlbumItemObject>()
            .expect("item is AlbumItemObject");
        let position = list_item.position();

        card.set_selection_mode(selection_mode.get());
        card.set_checked(list_item.is_selected());
        card.bind(&item);

        // Accessibility: label the card and its checkbox.
        let album = item.album();
        let card_label = if album.media_count == 1 {
            format!("{}, 1 photo", album.name)
        } else {
            format!("{}, {} photos", album.name, album.media_count)
        };
        card.update_property(&[gtk::accessible::Property::Label(&card_label)]);
        let checkbox_label = format!("{} {}", gettext("Select"), album.name);
        card.imp()
            .checkbox
            .update_property(&[gtk::accessible::Property::Label(&checkbox_label)]);

        // Wire checkbox → enter selection mode + select/deselect.
        {
            let checkbox = card.imp().checkbox.clone();
            let sel = selection.clone();
            let sm = Rc::clone(&selection_mode);
            let enter = enter_selection.clone();
            let handler_id = checkbox.connect_toggled(move |cb: &gtk::CheckButton| {
                if cb.is_active() {
                    if !sm.get() {
                        enter.activate(None);
                    }
                    sel.select_item(position, false);
                } else {
                    sel.unselect_item(position);
                }
            });
            card.imp()
                .checkbox_handler
                .borrow_mut()
                .replace(handler_id);
        }

        // Load mosaic thumbnails.
        if item.mosaic_texture(0).is_none() {
            let album_id = AlbumId::from_raw(item.album().id.as_str().to_owned());
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let item_weak = item.downgrade();

            gtk::glib::MainContext::default().spawn_local(async move {
                let lib_for_ids = Arc::clone(&lib);
                let aid = album_id.clone();
                let cover_ids = match tk
                    .spawn(async move { lib_for_ids.album_cover_media_ids(&aid, 4).await })
                    .await
                {
                    Ok(Ok(ids)) => ids,
                    Ok(Err(e)) => {
                        warn!("failed to fetch album cover IDs: {e}");
                        return;
                    }
                    Err(e) => {
                        warn!("tokio join error fetching cover IDs: {e}");
                        return;
                    }
                };

                if cover_ids.is_empty() {
                    return;
                }

                for (i, media_id) in cover_ids.into_iter().enumerate() {
                    let path = lib.thumbnail_path(&media_id);
                    let tk2 = tk.clone();
                    let item_weak2 = item_weak.clone();

                    let result = tk2
                        .spawn(async move {
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
                        .ok()
                        .flatten();

                    if let Some((pixels, width, height)) = result {
                        if let Some(item) = item_weak2.upgrade() {
                            let gbytes = gtk::glib::Bytes::from_owned(pixels);
                            let texture = gtk::gdk::MemoryTexture::new(
                                width as i32,
                                height as i32,
                                gtk::gdk::MemoryFormat::R8g8b8a8,
                                &gbytes,
                                (width as usize) * 4,
                            );
                            item.set_mosaic_texture(i, texture.upcast());
                        }
                    }
                }
            });
        }
    });

    factory.connect_unbind(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let card = list_item
            .child()
            .and_downcast::<AlbumCard>()
            .expect("child is AlbumCard");

        // Disconnect checkbox handler.
        if let Some(handler) = card.imp().checkbox_handler.borrow_mut().take() {
            card.imp().checkbox.disconnect(handler);
        }

        card.unbind();

        if let Some(item) = list_item
            .item()
            .and_then(|o| o.downcast::<AlbumItemObject>().ok())
        {
            item.set_texture0(None::<gtk::gdk::Texture>);
            item.set_texture1(None::<gtk::gdk::Texture>);
            item.set_texture2(None::<gtk::gdk::Texture>);
            item.set_texture3(None::<gtk::gdk::Texture>);
        }
    });

    factory.connect_teardown(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        list_item.set_child(None::<&gtk::Widget>);
    });

    factory
}

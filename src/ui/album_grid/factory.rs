use std::sync::Arc;

use gtk::prelude::*;

use crate::library::Library;

use super::card::AlbumCard;
use super::item::AlbumItemObject;

/// Build a `SignalListItemFactory` for the album grid.
pub fn build_factory(
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, obj| {
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

        card.bind(&item);

        // Load cover thumbnail asynchronously if not yet decoded.
        if item.texture().is_none() {
            if let Some(ref cover_id) = item.album().cover_media_id {
                let path = library.thumbnail_path(cover_id);
                let tk = tokio.clone();
                let item_weak = item.downgrade();

                gtk::glib::MainContext::default().spawn_local(async move {
                    let result = tk
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
                        if let Some(item) = item_weak.upgrade() {
                            let gbytes = gtk::glib::Bytes::from_owned(pixels);
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
    });

    factory.connect_unbind(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let card = list_item
            .child()
            .and_downcast::<AlbumCard>()
            .expect("child is AlbumCard");
        card.unbind();

        if let Some(item) = list_item
            .item()
            .and_then(|o| o.downcast::<AlbumItemObject>().ok())
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

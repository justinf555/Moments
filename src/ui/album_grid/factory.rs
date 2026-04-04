use std::sync::Arc;

use gtk::prelude::*;
use tracing::warn;

use crate::library::album::AlbumId;
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

        // Load mosaic thumbnails: fetch up to 4 cover IDs, then decode each.
        if item.mosaic_texture(0).is_none() {
            let album_id = AlbumId::from_raw(item.album().id.as_str().to_owned());
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let item_weak = item.downgrade();

            gtk::glib::MainContext::default().spawn_local(async move {
                // Fetch cover media IDs on Tokio.
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

                // Decode each thumbnail on a blocking thread.
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

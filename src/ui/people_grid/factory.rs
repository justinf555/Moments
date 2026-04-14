use gtk::{glib, prelude::*};

use crate::application::MomentsApplication;

use super::cell::PeopleGridCell;
use crate::client::PersonItemObject;

/// Build a `SignalListItemFactory` for the people grid.
pub fn build_factory(cell_size: i32) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let cell = PeopleGridCell::new();
        cell.set_size_request(cell_size, cell_size + 32); // Extra height for name label.
        list_item.set_child(Some(&cell));
    });

    factory.connect_bind(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let cell = list_item
            .child()
            .and_downcast::<PeopleGridCell>()
            .expect("child is PeopleGridCell");
        let item = list_item
            .item()
            .and_downcast::<PersonItemObject>()
            .expect("item is PersonItemObject");

        cell.bind(&item);

        // Decode thumbnail off the main thread to avoid scroll jank.
        if item.texture().is_none() {
            if let Some(path) = item.thumbnail_path() {
                let item_weak = item.downgrade();
                let tokio = MomentsApplication::default().tokio_handle();
                glib::MainContext::default().spawn_local(async move {
                    let result = tokio
                        .spawn_blocking(move || {
                            let data = std::fs::read(&path).ok()?;
                            let bytes = glib::Bytes::from_owned(data);
                            gtk::gdk::Texture::from_bytes(&bytes).ok()
                        })
                        .await
                        .ok()
                        .flatten();
                    if let Some(texture) = result {
                        if let Some(item) = item_weak.upgrade() {
                            item.set_texture(Some(&texture));
                        }
                    }
                });
            }
        }
    });

    factory.connect_unbind(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let cell = list_item
            .child()
            .and_downcast::<PeopleGridCell>()
            .expect("child is PeopleGridCell");
        cell.unbind();

        // Release texture.
        if let Some(item) = list_item
            .item()
            .and_then(|o| o.downcast::<PersonItemObject>().ok())
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

use gtk::prelude::*;

use super::cell::PeopleGridCell;
use super::item::PersonItemObject;

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

        // Load thumbnail from disk if not yet decoded.
        // Person thumbnails are small (250x250 JPEG) so loading on the
        // main thread is fine — no need for a blocking task.
        if item.texture().is_none() {
            if let Some(path) = &item.data().thumbnail_path {
                if let Ok(texture) = gtk::gdk::Texture::from_filename(path) {
                    item.set_texture(Some(&texture));
                }
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

use gtk::prelude::*;

use super::cell::PhotoGridCell;
use super::item::MediaItemObject;

/// Build the `SignalListItemFactory` for the photo grid.
///
/// In GTK 4.12+, factory callbacks receive `&glib::Object` which may be a
/// `ListItem` or a `ListHeader`. We downcast to `gtk::ListItem` explicitly.
///
/// `setup`    — creates a fresh `PhotoGridCell` and attaches it to the list item.
/// `bind`     — connects the cell to its `MediaItemObject`, reflecting current state.
/// `unbind`   — disconnects signals and resets the cell to its idle state.
/// `teardown` — removes the child widget so GTK can reclaim the list item slot.
pub fn build_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, obj| {
        let list_item = obj
            .downcast_ref::<gtk::ListItem>()
            .expect("is ListItem");
        list_item.set_child(Some(&PhotoGridCell::new()));
    });

    factory.connect_bind(|_, obj| {
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
        cell.bind(&item);
    });

    factory.connect_unbind(|_, obj| {
        let list_item = obj
            .downcast_ref::<gtk::ListItem>()
            .expect("is ListItem");
        let cell = list_item
            .child()
            .and_downcast::<PhotoGridCell>()
            .expect("child is PhotoGridCell");
        cell.unbind();
    });

    factory.connect_teardown(|_, obj| {
        let list_item = obj
            .downcast_ref::<gtk::ListItem>()
            .expect("is ListItem");
        list_item.set_child(None::<&gtk::Widget>);
    });

    factory
}

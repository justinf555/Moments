use std::sync::Arc;

use gtk::{glib, prelude::*, subclass::prelude::*};
use tracing::error;

use crate::library::Library;

use super::cell::PhotoGridCell;
use super::item::MediaItemObject;

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
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

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

            // Wire star button click → optimistic toggle + async persist.
            let star_btn = cell.imp().star_btn.clone();
            let item_weak = item.downgrade();
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let handler_id = star_btn.connect_clicked(move |_| {
                let Some(item) = item_weak.upgrade() else { return };
                let new_fav = !item.is_favorite();
                item.set_is_favorite(new_fav);

                let id = item.item().id.clone();
                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                glib::MainContext::default().spawn_local(async move {
                    let result = tk
                        .spawn(async move { lib.set_favorite(&[id], new_fav).await })
                        .await;
                    if let Ok(Err(e)) = result {
                        error!("set_favorite failed: {e}");
                    }
                });
            });

            // Store the handler ID so unbind can disconnect it.
            cell.imp()
                .star_click_handler
                .borrow_mut()
                .replace(handler_id);

            cell.bind(&item);
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
        // Disconnect star click before unbinding signals.
        if let Some(handler) = cell.imp().star_click_handler.borrow_mut().take() {
            cell.imp().star_btn.disconnect(handler);
        }
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

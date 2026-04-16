use std::cell::Cell;
use std::rc::Rc;

use gettextrs::gettext;
use gtk::{prelude::*, subclass::prelude::*};

use super::card::AlbumCard;
use crate::client::AlbumItemObject;

/// Build a `SignalListItemFactory` for the album grid.
///
/// Thumbnail loading is handled by `AlbumClientV2` via model patching —
/// the factory only binds card ↔ item and wires selection.
pub fn build_factory(
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
        let name = item.name();
        let count = item.media_count();
        let card_label = if count == 1 {
            format!("{name}, 1 photo")
        } else {
            format!("{name}, {count} photos")
        };
        card.update_property(&[gtk::accessible::Property::Label(&card_label)]);
        let checkbox_label = format!("{} {name}", gettext("Select"));
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
            card.imp().checkbox_handler.borrow_mut().replace(handler_id);
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
    });

    factory.connect_teardown(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        list_item.set_child(None::<&gtk::Widget>);
    });

    factory
}

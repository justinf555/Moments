use std::cell::Cell;
use std::rc::Rc;

use gettextrs::{gettext, ngettext};
use gtk::{gio, prelude::*, subclass::prelude::*};

use super::card::AlbumCard;
use crate::application::MomentsApplication;
use crate::client::AlbumItemObject;
use crate::library::album::AlbumId;
use crate::ui::album_dialogs;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::widgets::ContextMenuBin;

/// Build a `SignalListItemFactory` for the album grid.
///
/// Each card is wrapped in a `ContextMenuBin` for right-click support.
/// Thumbnail loading is handled by `AlbumClientV2` via model patching.
pub fn build_factory(
    selection_mode: Rc<Cell<bool>>,
    selection: gtk::MultiSelection,
    enter_selection: gio::SimpleAction,
    settings: gio::Settings,
    texture_cache: Rc<TextureCache>,
    bus_sender: crate::event_bus::EventSender,
    nav_view: adw::NavigationView,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let bin = ContextMenuBin::new();
        let card = AlbumCard::new();
        bin.set_child(Some(&card));
        list_item.set_child(Some(&bin));
    });

    factory.connect_bind(move |_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let bin = list_item
            .child()
            .and_downcast::<ContextMenuBin>()
            .expect("child is ContextMenuBin");
        let card = bin
            .child()
            .and_downcast::<AlbumCard>()
            .expect("bin child is AlbumCard");
        let item = list_item
            .item()
            .and_downcast::<AlbumItemObject>()
            .expect("item is AlbumItemObject");
        let position = list_item.position();

        card.set_selection_mode(selection_mode.get());
        card.set_checked(list_item.is_selected());
        card.bind(&item);

        // Accessibility labels.
        let name = item.name();
        let count = item.media_count();
        let photos = ngettext("{} photo", "{} photos", count).replace("{}", &count.to_string());
        card.update_property(&[gtk::accessible::Property::Label(&format!("{name}, {photos}"))]);
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

        // Set up per-item context menu on the ContextMenuBin.
        setup_context_menu(
            &bin,
            &item,
            &settings,
            &texture_cache,
            &bus_sender,
            &nav_view,
        );
    });

    factory.connect_unbind(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let bin = list_item
            .child()
            .and_downcast::<ContextMenuBin>()
            .expect("child is ContextMenuBin");
        let card = bin
            .child()
            .and_downcast::<AlbumCard>()
            .expect("bin child is AlbumCard");

        if let Some(handler) = card.imp().checkbox_handler.borrow_mut().take() {
            card.imp().checkbox.disconnect(handler);
        }
        card.unbind();
        bin.clear_context_menu("album");
    });

    factory.connect_teardown(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        list_item.set_child(None::<&gtk::Widget>);
    });

    factory
}

/// Build the context menu model and action group for a specific album item.
fn setup_context_menu(
    bin: &ContextMenuBin,
    item: &AlbumItemObject,
    settings: &gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
    nav_view: &adw::NavigationView,
) {
    let album_id_str = item.id();
    let album_name = item.name();
    let is_pinned = item.pinned();

    let album_client = MomentsApplication::default()
        .album_client_v2()
        .expect("album client v2 available");

    let action_group = gio::SimpleActionGroup::new();

    // Open
    let open_action = gio::SimpleAction::new("open", None);
    {
        let s = settings.clone();
        let tc = Rc::clone(texture_cache);
        let bs = bus_sender.clone();
        let nav = nav_view.clone();
        let aid = album_id_str.clone();
        let aname = album_name.clone();
        open_action.connect_activate(move |_, _| {
            super::actions::open_album_drilldown(
                &s,
                &tc,
                &bs,
                &nav,
                AlbumId::from_raw(aid.clone()),
                &aname,
            );
        });
    }
    action_group.add_action(&open_action);

    // Rename
    let rename_action = gio::SimpleAction::new("rename", None);
    {
        let ac = album_client.clone();
        let aid = album_id_str.clone();
        let aname = album_name.clone();
        let bin_weak = bin.downgrade();
        rename_action.connect_activate(move |_, _| {
            let ac = ac.clone();
            let aid = aid.clone();
            if let Some(bin) = bin_weak.upgrade() {
                if let Some(win) = bin.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    album_dialogs::show_rename_album_dialog(&win, &aname, move |new_name| {
                        ac.rename_album(AlbumId::from_raw(aid.clone()), new_name);
                    });
                }
            }
        });
    }
    action_group.add_action(&rename_action);

    // Pin
    let pin_action = gio::SimpleAction::new("pin", None);
    pin_action.set_enabled(!is_pinned);
    {
        let ac = album_client.clone();
        let aid = album_id_str.clone();
        pin_action.connect_activate(move |_, _| {
            tracing::debug!(album_id = %aid, "pin action activated");
            ac.pin_album(AlbumId::from_raw(aid.clone()));
        });
    }
    action_group.add_action(&pin_action);

    // Delete
    let delete_action = gio::SimpleAction::new("delete", None);
    {
        let ac = album_client.clone();
        let aid = album_id_str.clone();
        let aname = album_name.clone();
        let bin_weak = bin.downgrade();
        delete_action.connect_activate(move |_, _| {
            let ac = ac.clone();
            let aid = aid.clone();
            if let Some(bin) = bin_weak.upgrade() {
                if let Some(win) = bin.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    album_dialogs::show_delete_album_dialog(&win, &aname, move || {
                        ac.delete_album(vec![AlbumId::from_raw(aid.clone())]);
                    });
                }
            }
        });
    }
    action_group.add_action(&delete_action);

    // Menu model.
    let menu = gio::Menu::new();

    let main_section = gio::Menu::new();
    main_section.append(Some(&gettext("Open")), Some("album.open"));
    main_section.append(Some(&gettext("Rename\u{2026}")), Some("album.rename"));
    menu.append_section(None, &main_section);

    let pin_section = gio::Menu::new();
    let pin_label = if is_pinned {
        gettext("Pinned")
    } else {
        gettext("Pin to Sidebar")
    };
    pin_section.append(Some(&pin_label), Some("album.pin"));
    menu.append_section(None, &pin_section);

    let delete_section = gio::Menu::new();
    delete_section.append(Some(&gettext("Delete Album\u{2026}")), Some("album.delete"));
    menu.append_section(None, &delete_section);

    bin.set_context_menu(&menu.upcast(), action_group, "album");
}

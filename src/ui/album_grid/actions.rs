use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::gio;

use crate::application::MomentsApplication;
use crate::library::album::AlbumId;
use crate::library::media::MediaFilter;
use crate::ui::album_dialogs;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;

use crate::client::AlbumItemObject;

/// Push an album detail photo grid onto the navigation view.
///
/// Used by both item activation (double-click) and the context menu Open action.
pub(crate) fn open_album_drilldown(
    settings: &gtk::gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
    nav_view: &adw::NavigationView,
    album_id: AlbumId,
    album_name: &str,
) {
    let filter = MediaFilter::Album { album_id };
    let media_client = MomentsApplication::default()
        .media_client()
        .expect("media client available");
    let store = media_client.create_model(filter.clone());
    let view = PhotoGridView::new();
    view.setup(
        settings.clone(),
        Rc::clone(texture_cache),
        bus_sender.clone(),
    );
    view.set_store(store, filter);

    let page = adw::NavigationPage::builder()
        .tag("album-detail")
        .title(album_name)
        .child(&view)
        .build();

    nav_view.push(&page);
}

/// Find the clicked album item by resolving the cell's bound data.
/// Walks up from the picked widget to find the `AlbumCard`, then searches
/// the store for the matching item. Correct regardless of scroll position.
fn find_clicked_item(grid_view: &gtk::GridView, x: f64, y: f64) -> Option<AlbumItemObject> {
    let picked = grid_view.pick(x, y, gtk::PickFlags::DEFAULT)?;

    let mut widget = Some(picked);
    while let Some(ref w) = widget {
        if let Some(card) = w.downcast_ref::<super::card::AlbumCard>() {
            return card.bound_item();
        }
        widget = w.parent();
    }
    None
}

/// Build and show a right-click context menu for an album card.
///
/// Uses `PopoverMenu` with a `gio::Menu` model and per-item actions.
pub(crate) fn show_context_menu(
    grid_view: &gtk::GridView,
    settings: &gtk::gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
    nav_view: &adw::NavigationView,
    x: f64,
    y: f64,
) {
    let Some(obj) = find_clicked_item(grid_view, x, y) else {
        return;
    };

    let album_id_str = obj.id();
    let album_name = obj.name();
    let is_pinned = obj.pinned();

    let album_client = MomentsApplication::default()
        .album_client_v2()
        .expect("album client v2 available");

    // ── Actions ─────────────────────────────────────────────────────────
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
            open_album_drilldown(&s, &tc, &bs, &nav, AlbumId::from_raw(aid.clone()), &aname);
        });
    }
    action_group.add_action(&open_action);

    // Rename
    let rename_action = gio::SimpleAction::new("rename", None);
    {
        let ac = album_client.clone();
        let aid = album_id_str.clone();
        let aname = album_name.clone();
        let gv = grid_view.clone();
        rename_action.connect_activate(move |_, _| {
            let ac = ac.clone();
            let aid = aid.clone();
            if let Some(win) = gv.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                album_dialogs::show_rename_album_dialog(&win, &aname, move |new_name| {
                    ac.rename_album(AlbumId::from_raw(aid.clone()), new_name);
                });
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
        let gv = grid_view.clone();
        delete_action.connect_activate(move |_, _| {
            let ac = ac.clone();
            let aid = aid.clone();
            if let Some(win) = gv.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                album_dialogs::show_delete_album_dialog(&win, &aname, move || {
                    ac.delete_album(vec![AlbumId::from_raw(aid.clone())]);
                });
            }
        });
    }
    action_group.add_action(&delete_action);

    // ── Menu model ──────────────────────────────────────────────────────
    let menu = gio::Menu::new();

    let main_section = gio::Menu::new();
    main_section.append(Some(&gettext("Open")), Some("ctx.open"));
    main_section.append(Some(&gettext("Rename\u{2026}")), Some("ctx.rename"));
    menu.append_section(None, &main_section);

    let pin_section = gio::Menu::new();
    let pin_label = if is_pinned {
        gettext("Pinned")
    } else {
        gettext("Pin to Sidebar")
    };
    pin_section.append(Some(&pin_label), Some("ctx.pin"));
    menu.append_section(None, &pin_section);

    let delete_section = gio::Menu::new();
    delete_section.append(Some(&gettext("Delete Album\u{2026}")), Some("ctx.delete"));
    menu.append_section(None, &delete_section);

    // ── Popover ─────────────────────────────────────────────────────────
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(grid_view);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.set_has_arrow(true);
    // Install action group on the grid_view so GTK's action resolution
    // finds it when walking up from the PopoverMenu.
    grid_view.insert_action_group("ctx", Some(&action_group));

    let gv = grid_view.clone();
    popover.connect_closed(move |p| {
        gv.insert_action_group("ctx", None::<&gio::SimpleActionGroup>);
        p.unparent();
    });
    popover.popup();
}

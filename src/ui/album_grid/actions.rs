use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;

use crate::application::MomentsApplication;
use crate::client::AlbumClientV2;
use crate::library::album::AlbumId;
use crate::library::media::MediaFilter;
use crate::ui::album_dialogs;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;

use crate::client::AlbumItemObject;

/// Push an album detail photo grid onto the navigation view.
///
/// Used by both item activation (double-click) and the context menu Open button,
/// eliminating previously duplicated drill-down logic.
pub(crate) fn open_album_drilldown(
    settings: &gtk::gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
    nav_view: &adw::NavigationView,
    album_id: AlbumId,
    album_name: &str,
) {
    let filter = MediaFilter::Album { album_id };
    let media_client = crate::application::MomentsApplication::default()
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

/// Build and show a right-click context menu popover for an album card.
///
/// Resolves the clicked item from (x, y), then builds a popover with
/// Open, Rename, Pin to Sidebar, Share (stub), and Delete actions.
pub(crate) fn show_context_menu(
    grid_view: &gtk::GridView,
    settings: &gtk::gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
    nav_view: &adw::NavigationView,
    x: f64,
    y: f64,
) {
    // Find which grid item was clicked by resolving the cell's bound data.
    // This is correct even when the grid is scrolled (GridView is virtualized).
    let Some(obj) = find_clicked_item(grid_view, x, y) else {
        return;
    };

    let album_id_str = obj.id();
    let album_name = obj.name();

    // Build popover menu.
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.set_margin_top(6);
    vbox.set_margin_bottom(6);
    vbox.set_margin_start(6);
    vbox.set_margin_end(6);

    let popover = gtk::Popover::new();

    // Open
    let open_btn = gtk::Button::with_label(&gettext("Open"));
    open_btn.add_css_class("flat");
    vbox.append(&open_btn);

    // Rename
    let rename_btn = gtk::Button::with_label(&gettext("Rename…"));
    rename_btn.add_css_class("flat");
    vbox.append(&rename_btn);

    // Separator
    vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    // Pin to sidebar.
    let pin_btn = gtk::Button::with_label(&gettext("Pin to Sidebar"));
    pin_btn.add_css_class("flat");
    configure_pin_button(&pin_btn, &obj);
    vbox.append(&pin_btn);

    // Share (stub)
    let share_btn = gtk::Button::with_label(&gettext("Share…"));
    share_btn.add_css_class("flat");
    share_btn.set_sensitive(false);
    vbox.append(&share_btn);

    // Separator
    vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    // Delete (destructive)
    let delete_btn = gtk::Button::with_label(&gettext("Delete Album…"));
    delete_btn.add_css_class("flat");
    delete_btn.add_css_class("error");
    vbox.append(&delete_btn);

    popover.set_child(Some(&vbox));
    popover.set_parent(grid_view);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.set_has_arrow(true);

    let album_client = MomentsApplication::default()
        .album_client_v2()
        .expect("album client v2 available");

    // Wire Pin to sidebar.
    wire_pin_button(&pin_btn, &popover, &album_client, &album_id_str);

    // Wire Open.
    wire_open_button(
        &open_btn,
        &popover,
        settings,
        texture_cache,
        bus_sender,
        nav_view,
        &album_id_str,
        &album_name,
    );

    // Wire Rename.
    wire_rename_button(
        &rename_btn,
        &popover,
        &album_client,
        grid_view,
        &album_id_str,
        &album_name,
    );

    // Wire Delete.
    wire_delete_button(
        &delete_btn,
        &popover,
        &album_client,
        grid_view,
        &album_id_str,
        &album_name,
    );

    popover.connect_closed(|p| {
        p.unparent();
    });
    popover.popup();
}

/// Check pin state and disable the button if already pinned.
fn configure_pin_button(pin_btn: &gtk::Button, obj: &crate::client::AlbumItemObject) {
    if obj.pinned() {
        pin_btn.set_label(&gettext("Pinned"));
        pin_btn.set_sensitive(false);
    }
}

/// Wire the Pin to Sidebar button click handler.
fn wire_pin_button(
    pin_btn: &gtk::Button,
    popover: &gtk::Popover,
    album_client: &AlbumClientV2,
    album_id_str: &str,
) {
    let pop = popover.downgrade();
    let ac = album_client.clone();
    let aid = album_id_str.to_owned();
    pin_btn.connect_clicked(move |_| {
        if let Some(p) = pop.upgrade() {
            p.popdown();
        }
        ac.pin_album(AlbumId::from_raw(aid.clone()));
    });
}

/// Wire the Open button to push an album detail view.
#[allow(clippy::too_many_arguments)]
fn wire_open_button(
    open_btn: &gtk::Button,
    popover: &gtk::Popover,
    settings: &gtk::gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
    nav_view: &adw::NavigationView,
    album_id_str: &str,
    album_name: &str,
) {
    let pop = popover.downgrade();
    let s = settings.clone();
    let tc = Rc::clone(texture_cache);
    let bs = bus_sender.clone();
    let nav = nav_view.clone();
    let aid = album_id_str.to_owned();
    let aname = album_name.to_owned();
    open_btn.connect_clicked(move |_| {
        if let Some(p) = pop.upgrade() {
            p.popdown();
        }
        let album_id = AlbumId::from_raw(aid.clone());
        open_album_drilldown(&s, &tc, &bs, &nav, album_id, &aname);
    });
}

/// Wire the Rename button to show a rename dialog.
fn wire_rename_button(
    rename_btn: &gtk::Button,
    popover: &gtk::Popover,
    album_client: &AlbumClientV2,
    grid_view: &gtk::GridView,
    album_id_str: &str,
    album_name: &str,
) {
    let pop = popover.downgrade();
    let ac = album_client.clone();
    let aid = album_id_str.to_owned();
    let aname = album_name.to_owned();
    let gv_ref = grid_view.clone();
    rename_btn.connect_clicked(move |_| {
        if let Some(p) = pop.upgrade() {
            p.popdown();
        }
        let ac = ac.clone();
        let aid = aid.clone();
        if let Some(win) = gv_ref.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
            album_dialogs::show_rename_album_dialog(&win, &aname, move |new_name| {
                ac.rename_album(AlbumId::from_raw(aid.clone()), new_name);
            });
        }
    });
}

/// Wire the Delete button to show a confirmation dialog.
fn wire_delete_button(
    delete_btn: &gtk::Button,
    popover: &gtk::Popover,
    album_client: &AlbumClientV2,
    grid_view: &gtk::GridView,
    album_id_str: &str,
    album_name: &str,
) {
    let pop = popover.downgrade();
    let ac = album_client.clone();
    let aid = album_id_str.to_owned();
    let aname = album_name.to_owned();
    let gv_ref = grid_view.clone();
    delete_btn.connect_clicked(move |_| {
        if let Some(p) = pop.upgrade() {
            p.popdown();
        }
        let ac = ac.clone();
        let aid = aid.clone();
        if let Some(win) = gv_ref.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
            album_dialogs::show_delete_album_dialog(&win, &aname, move || {
                ac.delete_album(vec![AlbumId::from_raw(aid.clone())]);
            });
        }
    });
}

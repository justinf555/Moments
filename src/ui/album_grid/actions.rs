use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;

use tracing::debug;

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

    // ── Build popover ──────────────────────────────────────────────────
    // TODO: Investigate PopoverMenu + gio::Menu action resolution with
    // GridView. Manual popover works; PopoverMenu actions don't resolve.
    // See Fractal's ContextMenuBin pattern for the proper approach.
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.set_margin_top(6);
    vbox.set_margin_bottom(6);
    vbox.set_margin_start(6);
    vbox.set_margin_end(6);

    let popover = gtk::Popover::new();

    let open_btn = gtk::Button::with_label(&gettext("Open"));
    open_btn.add_css_class("flat");
    vbox.append(&open_btn);

    let rename_btn = gtk::Button::with_label(&gettext("Rename\u{2026}"));
    rename_btn.add_css_class("flat");
    vbox.append(&rename_btn);

    vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let pin_label = if is_pinned { gettext("Pinned") } else { gettext("Pin to Sidebar") };
    let pin_btn = gtk::Button::with_label(&pin_label);
    pin_btn.add_css_class("flat");
    if is_pinned { pin_btn.set_sensitive(false); }
    vbox.append(&pin_btn);

    vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let delete_btn = gtk::Button::with_label(&gettext("Delete Album\u{2026}"));
    delete_btn.add_css_class("flat");
    delete_btn.add_css_class("error");
    vbox.append(&delete_btn);

    popover.set_child(Some(&vbox));
    popover.set_parent(grid_view);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.set_has_arrow(true);

    // ── Wire handlers ──────────────────────────────────────────────────
    {
        let pop = popover.downgrade();
        let s = settings.clone();
        let tc = Rc::clone(texture_cache);
        let bs = bus_sender.clone();
        let nav = nav_view.clone();
        let aid = album_id_str.clone();
        let aname = album_name.clone();
        open_btn.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() { p.popdown(); }
            open_album_drilldown(&s, &tc, &bs, &nav, AlbumId::from_raw(aid.clone()), &aname);
        });
    }
    {
        let pop = popover.downgrade();
        let ac = album_client.clone();
        let aid = album_id_str.clone();
        let aname = album_name.clone();
        let gv = grid_view.clone();
        rename_btn.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() { p.popdown(); }
            let ac = ac.clone();
            let aid = aid.clone();
            if let Some(win) = gv.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                album_dialogs::show_rename_album_dialog(&win, &aname, move |new_name| {
                    ac.rename_album(AlbumId::from_raw(aid.clone()), new_name);
                });
            }
        });
    }
    {
        let pop = popover.downgrade();
        let ac = album_client.clone();
        let aid = album_id_str.clone();
        pin_btn.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() { p.popdown(); }
            debug!(album_id = %aid, "pin action activated");
            ac.pin_album(AlbumId::from_raw(aid.clone()));
        });
    }
    {
        let pop = popover.downgrade();
        let ac = album_client.clone();
        let aid = album_id_str.clone();
        let aname = album_name.clone();
        let gv = grid_view.clone();
        delete_btn.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() { p.popdown(); }
            let ac = ac.clone();
            let aid = aid.clone();
            if let Some(win) = gv.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                album_dialogs::show_delete_album_dialog(&win, &aname, move || {
                    ac.delete_album(vec![AlbumId::from_raw(aid.clone())]);
                });
            }
        });
    }

    popover.connect_closed(|p| { p.unparent(); });
    popover.popup();
}

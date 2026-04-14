use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{glib, subclass::prelude::*};
use tracing::debug;

use crate::library::album::AlbumId;
use crate::library::media::MediaFilter;
use crate::library::Library;
use crate::ui::album_dialogs;
use crate::ui::photo_grid::model::PhotoGridModel;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;

use super::item::AlbumItemObject;

/// Push an album detail photo grid onto the navigation view.
///
/// Used by both item activation (double-click) and the context menu Open button,
/// eliminating previously duplicated drill-down logic.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_album_drilldown(
    library: &Arc<Library>,
    tokio: &tokio::runtime::Handle,
    settings: &gtk::gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
    nav_view: &adw::NavigationView,
    album_id: AlbumId,
    album_name: &str,
) {
    let model = PhotoGridModel::new(
        Arc::clone(library),
        tokio.clone(),
        MediaFilter::Album { album_id },
        bus_sender.clone(),
    );
    let view = PhotoGridView::new();
    view.setup(
        Arc::clone(library),
        tokio.clone(),
        settings.clone(),
        Rc::clone(texture_cache),
        bus_sender.clone(),
    );
    view.set_model(model.clone());

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
#[allow(clippy::too_many_arguments)]
pub(crate) fn show_context_menu(
    grid_view: &gtk::GridView,
    library: &Arc<Library>,
    tokio: &tokio::runtime::Handle,
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

    let album = obj.album();
    let album_id_str = album.id.as_str().to_owned();
    let album_name = album.name.clone();

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
    configure_pin_button(&pin_btn, &album_id_str);
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

    // Wire Pin to sidebar.
    wire_pin_button(&pin_btn, &popover, &album_id_str, &album_name);

    // Wire Open.
    wire_open_button(
        &open_btn,
        &popover,
        library,
        tokio,
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
        library,
        tokio,
        bus_sender,
        grid_view,
        &album_id_str,
        &album_name,
    );

    // Wire Delete.
    wire_delete_button(
        &delete_btn,
        &popover,
        bus_sender,
        grid_view,
        &album_id_str,
        &album_name,
    );

    popover.connect_closed(|p| {
        p.unparent();
    });
    popover.popup();
}

/// Check pin state and disable the button if already pinned or at limit.
fn configure_pin_button(pin_btn: &gtk::Button, album_id_str: &str) {
    // TODO: replace widget-tree walk with AppEvent bus pattern.
    let app = crate::application::MomentsApplication::default();
    if let Some(win) = app.active_window() {
        if let Some(win) = win.downcast_ref::<crate::ui::MomentsWindow>() {
            if let Some(sb) = win.sidebar() {
                if sb.is_pinned(album_id_str) {
                    pin_btn.set_label(&gettext("Pinned"));
                    pin_btn.set_sensitive(false);
                } else if sb.pinned_count() >= 5 {
                    pin_btn.set_sensitive(false);
                    pin_btn.set_tooltip_text(Some(&gettext("Unpin an album to pin another")));
                }
            }
        }
    }
}

/// Wire the Pin to Sidebar button click handler.
fn wire_pin_button(
    pin_btn: &gtk::Button,
    popover: &gtk::Popover,
    album_id_str: &str,
    album_name: &str,
) {
    // TODO: replace widget-tree walk with AppEvent bus pattern.
    let pop = popover.downgrade();
    let aid = album_id_str.to_owned();
    let aname = album_name.to_owned();
    pin_btn.connect_clicked(move |_| {
        if let Some(p) = pop.upgrade() {
            p.popdown();
        }
        let app = crate::application::MomentsApplication::default();
        if let Some(settings) = app.imp().settings.get() {
            if let Some(win) = app.active_window() {
                if let Some(win) = win.downcast_ref::<crate::ui::MomentsWindow>() {
                    if let Some(sb) = win.sidebar() {
                        sb.pin_album(&aid, &aname, settings);
                    }
                }
            }
        }
    });
}

/// Wire the Open button to push an album detail view.
#[allow(clippy::too_many_arguments)]
fn wire_open_button(
    open_btn: &gtk::Button,
    popover: &gtk::Popover,
    library: &Arc<Library>,
    tokio: &tokio::runtime::Handle,
    settings: &gtk::gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
    nav_view: &adw::NavigationView,
    album_id_str: &str,
    album_name: &str,
) {
    let pop = popover.downgrade();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
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
        open_album_drilldown(&lib, &tk, &s, &tc, &bs, &nav, album_id, &aname);
    });
}

/// Wire the Rename button to show a rename dialog.
#[allow(clippy::too_many_arguments)]
fn wire_rename_button(
    rename_btn: &gtk::Button,
    popover: &gtk::Popover,
    library: &Arc<Library>,
    tokio: &tokio::runtime::Handle,
    bus_sender: &crate::event_bus::EventSender,
    grid_view: &gtk::GridView,
    album_id_str: &str,
    album_name: &str,
) {
    let pop = popover.downgrade();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let bs = bus_sender.clone();
    let aid = album_id_str.to_owned();
    let aname = album_name.to_owned();
    let gv_ref = grid_view.clone();
    rename_btn.connect_clicked(move |_| {
        if let Some(p) = pop.upgrade() {
            p.popdown();
        }
        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let bs = bs.clone();
        let aid = aid.clone();
        if let Some(win) = gv_ref.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
            album_dialogs::show_rename_album_dialog(&win, &aname, move |new_name| {
                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                let bs = bs.clone();
                let aid = aid.clone();
                glib::MainContext::default().spawn_local(async move {
                    let n = new_name.clone();
                    let id = AlbumId::from_raw(aid.clone());
                    match tk
                        .spawn(async move { lib.rename_album(&id, &n).await })
                        .await
                    {
                        Ok(Ok(())) => {
                            debug!(album_id = %aid, name = %new_name, "album renamed");
                            bs.send(crate::app_event::AppEvent::AlbumRenamed {
                                id: AlbumId::from_raw(aid),
                                name: new_name,
                            });
                        }
                        Ok(Err(e)) => {
                            tracing::error!("failed to rename album: {e}");
                            bs.send(crate::app_event::AppEvent::Error(format!(
                                "Failed to rename album: {e}"
                            )));
                        }
                        Err(e) => {
                            tracing::error!("tokio join error: {e}");
                            bs.send(crate::app_event::AppEvent::Error(format!(
                                "Failed to rename album: {e}"
                            )));
                        }
                    }
                });
            });
        }
    });
}

/// Wire the Delete button to show a confirmation dialog.
fn wire_delete_button(
    delete_btn: &gtk::Button,
    popover: &gtk::Popover,
    bus_sender: &crate::event_bus::EventSender,
    grid_view: &gtk::GridView,
    album_id_str: &str,
    album_name: &str,
) {
    let pop = popover.downgrade();
    let bs = bus_sender.clone();
    let aid = album_id_str.to_owned();
    let aname = album_name.to_owned();
    let gv_ref = grid_view.clone();
    delete_btn.connect_clicked(move |_| {
        if let Some(p) = pop.upgrade() {
            p.popdown();
        }
        let bs = bs.clone();
        let aid = aid.clone();
        if let Some(win) = gv_ref.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
            album_dialogs::show_delete_album_dialog(&win, &aname, move || {
                bs.send(crate::app_event::AppEvent::DeleteAlbumRequested {
                    ids: vec![AlbumId::from_raw(aid.clone())],
                });
            });
        }
    });
}

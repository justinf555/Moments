//! Context menu and album popover wiring for the photo grid.

use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use tracing::debug;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::media::MediaFilter;
use crate::library::Library;

use super::item::MediaItemObject;

/// Context passed to wiring functions.
///
/// Carries the bus sender for commands and the library/tokio for
/// read-only queries (e.g. list_albums for the album popover).
pub(super) struct ActionContext {
    pub selection: gtk::MultiSelection,
    pub library: Arc<dyn Library>,
    pub tokio: tokio::runtime::Handle,
    pub filter: MediaFilter,
    pub grid_view: gtk::GridView,
    pub bus_sender: EventSender,
}

/// Wire the "Add to Album" popover on the given button.
///
/// The popover lists existing albums (fetched via library query) and
/// a "New Album..." option. All mutations go through the bus.
pub(super) fn wire_album_controls(ctx: &ActionContext, album_btn: &gtk::Button) {
    let lib = Arc::clone(&ctx.library);
    let tk = ctx.tokio.clone();
    let selection = ctx.selection.clone();
    let bus_tx = ctx.bus_sender.clone();

    album_btn.connect_clicked(move |btn: &gtk::Button| {
        debug!("album button clicked, loading albums async");

        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let sel = selection.clone();
        let bus_tx = bus_tx.clone();
        let btn_weak: glib::WeakRef<gtk::Button> = btn.downgrade();

        glib::MainContext::default().spawn_local(async move {
            let lib_q = Arc::clone(&lib);
            let albums = match tk.spawn(async move { lib_q.list_albums().await }).await {
                Ok(Ok(a)) => a,
                Ok(Err(e)) => {
                    tracing::error!("list_albums failed: {e}");
                    return;
                }
                Err(e) => {
                    tracing::error!("list_albums join failed: {e}");
                    return;
                }
            };

            let Some(btn) = btn_weak.upgrade() else { return };

            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
            vbox.set_margin_top(6);
            vbox.set_margin_bottom(6);
            vbox.set_margin_start(6);
            vbox.set_margin_end(6);

            let popover = gtk::Popover::new();
            popover.set_parent(btn.upcast_ref::<gtk::Widget>());

            if albums.is_empty() {
                let label = gtk::Label::new(Some("No albums"));
                label.add_css_class("dim-label");
                vbox.append(&label);
            } else {
                for album in &albums {
                    let ab = gtk::Button::with_label(&album.name);
                    ab.add_css_class("flat");
                    let aid = album.id.clone();
                    let sel_add = sel.clone();
                    let tx = bus_tx.clone();
                    let pop_weak = popover.downgrade();
                    ab.connect_clicked(move |_| {
                        let ids = super::collect_selected_ids(&sel_add);
                        if ids.is_empty() { return; }
                        if let Some(p) = pop_weak.upgrade() {
                            p.popdown();
                        }
                        tx.send(AppEvent::AddToAlbumRequested {
                            album_id: aid.clone(),
                            ids,
                        });
                    });
                    vbox.append(&ab);
                }
            }

            // Separator + "New Album..." button.
            let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
            sep.set_margin_top(4);
            sep.set_margin_bottom(4);
            vbox.append(&sep);

            let new_album_btn = gtk::Button::with_label("New Album…");
            new_album_btn.add_css_class("flat");
            {
                let pop_weak = popover.downgrade();
                let sel_new = sel.clone();
                let tx = bus_tx.clone();
                new_album_btn.connect_clicked(move |btn| {
                    if let Some(p) = pop_weak.upgrade() {
                        p.popdown();
                    }

                    let dialog = adw::AlertDialog::builder()
                        .heading("New Album")
                        .build();
                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("create", "Create");
                    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
                    dialog.set_default_response(Some("create"));
                    dialog.set_close_response("cancel");

                    let entry = gtk::Entry::new();
                    entry.set_placeholder_text(Some("Album name"));
                    entry.set_activates_default(true);
                    dialog.set_extra_child(Some(&entry));

                    let sel = sel_new.clone();
                    let tx = tx.clone();
                    dialog.connect_response(None, move |_, response| {
                        if response != "create" { return; }
                        let name = entry.text().to_string();
                        if name.is_empty() { return; }
                        let ids = super::collect_selected_ids(&sel);
                        tx.send(AppEvent::CreateAlbumRequested { name, ids });
                    });

                    dialog.present(
                        btn.root()
                            .as_ref()
                            .and_then(|r| r.downcast_ref::<gtk::Window>()),
                    );
                });
            }
            vbox.append(&new_album_btn);

            popover.set_child(Some(&vbox));
            popover.connect_closed(move |p| { p.unparent(); });
            popover.popup();
        });
    });
}

/// Wire the right-click context menu on grid cells.
///
/// All actions emit command events via the bus — no direct library calls.
pub(super) fn wire_context_menu(ctx: &ActionContext) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);

    let grid_view = ctx.grid_view.clone();
    let selection = ctx.selection.clone();
    let filter = ctx.filter.clone();
    let bus_tx = ctx.bus_sender.clone();

    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(picked) = grid_view.pick(x, y, gtk::PickFlags::DEFAULT) else {
            return;
        };

        // Walk up from the picked widget to find the grid cell.
        let grid_widget = grid_view.upcast_ref::<gtk::Widget>();
        let mut target = Some(picked);
        while let Some(ref w) = target {
            if w.parent().as_ref() == Some(grid_widget) {
                break;
            }
            target = w.parent();
        }
        let Some(target) = target else { return };

        // Find position of the cell.
        let mut pos = 0u32;
        let mut child = grid_view.first_child();
        loop {
            match child {
                Some(ref c) if !c.eq(&target) => {
                    pos += 1;
                    child = c.next_sibling();
                }
                _ => break,
            }
        }

        // Select the right-clicked item.
        if selection.selection().size() <= 1 {
            selection.select_item(pos, true);
        }

        let Some(obj) = selection
            .item(pos)
            .and_then(|o| o.downcast::<MediaItemObject>().ok())
        else {
            return;
        };

        let is_favorite = obj.is_favorite();
        let is_trash = matches!(filter, MediaFilter::Trashed);
        let is_album = matches!(filter, MediaFilter::Album { .. });

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.set_margin_top(6);
        vbox.set_margin_bottom(6);
        vbox.set_margin_start(6);
        vbox.set_margin_end(6);

        let popover = gtk::Popover::new();
        let pop_ref: glib::WeakRef<gtk::Popover> = popover.downgrade();

        if is_trash {
            // ── Trash context menu: Restore, Delete Permanently ─────────
            let restore_btn = gtk::Button::with_label("Restore");
            restore_btn.add_css_class("flat");
            vbox.append(&restore_btn);

            let delete_btn = gtk::Button::with_label("Delete Permanently");
            delete_btn.add_css_class("flat");
            delete_btn.add_css_class("error");
            vbox.append(&delete_btn);

            {
                let pw = pop_ref.clone();
                let sel = selection.clone();
                let tx = bus_tx.clone();
                restore_btn.connect_clicked(move |_| {
                    if let Some(p) = pw.upgrade() { p.popdown(); }
                    let ids = super::collect_selected_ids(&sel);
                    if !ids.is_empty() {
                        tx.send(AppEvent::RestoreRequested { ids });
                    }
                });
            }
            {
                let pw = pop_ref.clone();
                let sel = selection.clone();
                let tx = bus_tx.clone();
                delete_btn.connect_clicked(move |_| {
                    if let Some(p) = pw.upgrade() { p.popdown(); }
                    let ids = super::collect_selected_ids(&sel);
                    if !ids.is_empty() {
                        tx.send(AppEvent::DeleteRequested { ids });
                    }
                });
            }
        } else {
            // ── Standard/Album context menu ─────────────────────────────
            let fav_label = if is_favorite { "Unfavourite" } else { "Favourite" };
            let fav_btn = gtk::Button::with_label(fav_label);
            fav_btn.add_css_class("flat");
            vbox.append(&fav_btn);

            let trash_btn = gtk::Button::with_label("Move to Trash");
            trash_btn.add_css_class("flat");
            trash_btn.add_css_class("error");
            vbox.append(&trash_btn);

            if is_album {
                let remove_btn = gtk::Button::with_label("Remove from Album");
                remove_btn.add_css_class("flat");
                vbox.append(&remove_btn);

                if let MediaFilter::Album { ref album_id } = filter {
                    let pw = pop_ref.clone();
                    let sel = selection.clone();
                    let tx = bus_tx.clone();
                    let aid = album_id.clone();
                    remove_btn.connect_clicked(move |_| {
                        if let Some(p) = pw.upgrade() { p.popdown(); }
                        let ids = super::collect_selected_ids(&sel);
                        if !ids.is_empty() {
                            tx.send(AppEvent::RemoveFromAlbumRequested {
                                album_id: aid.clone(),
                                ids,
                            });
                        }
                    });
                }
            }

            let new_fav = !is_favorite;
            {
                let pw = pop_ref.clone();
                let sel = selection.clone();
                let tx = bus_tx.clone();
                fav_btn.connect_clicked(move |_| {
                    if let Some(p) = pw.upgrade() { p.popdown(); }
                    let ids = super::collect_selected_ids(&sel);
                    if !ids.is_empty() {
                        tx.send(AppEvent::FavoriteRequested { ids, state: new_fav });
                    }
                });
            }
            {
                let pw = pop_ref.clone();
                let sel = selection.clone();
                let tx = bus_tx.clone();
                trash_btn.connect_clicked(move |_| {
                    if let Some(p) = pw.upgrade() { p.popdown(); }
                    let ids = super::collect_selected_ids(&sel);
                    if !ids.is_empty() {
                        tx.send(AppEvent::TrashRequested { ids });
                    }
                });
            }
        }

        popover.set_child(Some(&vbox));
        popover.set_parent(&grid_view);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            x as i32, y as i32, 1, 1,
        )));
        popover.set_has_arrow(true);

        popover.connect_closed(move |p| {
            p.unparent();
        });

        popover.popup();
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });

    ctx.grid_view.add_controller(gesture);
}

/// Update the favourite button's icon and label to reflect the current state.
/// `all_fav = true` means all selected items are favourited → show "Unfavourite".
pub(super) fn update_fav_button(btn: &gtk::Button, all_fav: bool) {
    let Some(content) = btn.child().and_downcast::<gtk::Box>() else { return };
    let Some(icon) = content.first_child().and_downcast::<gtk::Image>() else { return };
    let Some(label) = icon.next_sibling().and_downcast::<gtk::Label>() else { return };

    if all_fav {
        icon.set_icon_name(Some("non-starred-symbolic"));
        label.set_label("Unfavourite");
    } else {
        icon.set_icon_name(Some("starred-symbolic"));
        label.set_label("Favourite");
    }
}

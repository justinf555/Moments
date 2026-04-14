//! Context menu and album popover wiring for the photo grid.

use adw::prelude::*;
use gtk::glib;
use tracing::debug;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::media::MediaFilter;

use super::item::MediaItemObject;

/// Context passed to wiring functions.
///
/// Carries the bus sender for commands and view references for
/// context menus and action bar actions.
pub(super) struct ActionContext {
    pub selection: gtk::MultiSelection,
    pub filter: MediaFilter,
    pub grid_view: gtk::GridView,
    pub bus_sender: EventSender,
}

/// Wire the "Add to Album" button to open the album picker dialog.
pub(super) fn wire_album_controls(ctx: &ActionContext, album_btn: &gtk::Button) {
    let selection = ctx.selection.clone();
    let bus_tx = ctx.bus_sender.clone();

    album_btn.connect_clicked(move |btn: &gtk::Button| {
        debug!("album button clicked");
        let ids = super::collect_selected_ids(&selection);
        if ids.is_empty() {
            return;
        }
        crate::ui::album_picker_dialog::show_album_picker_dialog(btn, ids, bus_tx.clone());
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
        let Some(pos) = find_clicked_position(&grid_view, &selection, x, y) else {
            return;
        };

        // If the right-clicked item is not already selected, select just it
        // (replacing any existing selection). If it's already part of a
        // multi-selection, preserve the selection so the context menu acts
        // on all selected items.
        if !selection.is_selected(pos) {
            selection.unselect_all();
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

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.set_margin_top(6);
        vbox.set_margin_bottom(6);
        vbox.set_margin_start(6);
        vbox.set_margin_end(6);

        let popover = gtk::Popover::new();
        let pop_ref: glib::WeakRef<gtk::Popover> = popover.downgrade();

        if is_trash {
            build_trash_menu(&vbox, &pop_ref, &selection, &bus_tx);
        } else {
            build_standard_menu(&vbox, &pop_ref, &selection, &bus_tx, &filter, is_favorite);
        }

        popover.set_child(Some(&vbox));
        popover.set_parent(&grid_view);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.set_has_arrow(true);

        popover.connect_closed(move |p| {
            p.unparent();
        });

        popover.popup();
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });

    ctx.grid_view.add_controller(gesture);
}

/// Find the store position of the item at (x, y) by resolving the cell's
/// bound data. This is correct even when the grid is scrolled (unlike
/// counting siblings, which only works for non-virtualized lists).
fn find_clicked_position(
    grid_view: &gtk::GridView,
    selection: &gtk::MultiSelection,
    x: f64,
    y: f64,
) -> Option<u32> {
    let picked = grid_view.pick(x, y, gtk::PickFlags::DEFAULT)?;

    // Walk up from the picked widget to find the PhotoGridCell.
    let mut widget = Some(picked);
    while let Some(ref w) = widget {
        if let Some(cell) = w.downcast_ref::<super::cell::PhotoGridCell>() {
            let item = cell.bound_item()?;
            let target_id = item.item().id.clone();
            // Search the selection model for the matching item.
            for i in 0..selection.n_items() {
                if let Some(obj) = selection
                    .item(i)
                    .and_then(|o| o.downcast::<MediaItemObject>().ok())
                {
                    if obj.item().id == target_id {
                        return Some(i);
                    }
                }
            }
            return None;
        }
        widget = w.parent();
    }
    None
}

/// Build the trash-view context menu: Restore, Delete Permanently.
fn build_trash_menu(
    vbox: &gtk::Box,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    selection: &gtk::MultiSelection,
    bus_tx: &EventSender,
) {
    let restore_btn = gtk::Button::with_label("Restore");
    restore_btn.add_css_class("flat");
    vbox.append(&restore_btn);

    let delete_btn = gtk::Button::with_label("Delete Permanently");
    delete_btn.add_css_class("flat");
    delete_btn.add_css_class("error");
    vbox.append(&delete_btn);

    wire_restore_button(&restore_btn, pop_ref, selection, bus_tx);
    wire_permanent_delete_button(&delete_btn, pop_ref, selection, bus_tx);
}

/// Build the standard/album context menu: Favourite, Move to Trash,
/// and optionally Remove from Album.
fn build_standard_menu(
    vbox: &gtk::Box,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    selection: &gtk::MultiSelection,
    bus_tx: &EventSender,
    filter: &MediaFilter,
    is_favorite: bool,
) {
    let fav_label = if is_favorite {
        "Unfavourite"
    } else {
        "Favourite"
    };
    let fav_btn = gtk::Button::with_label(fav_label);
    fav_btn.add_css_class("flat");
    vbox.append(&fav_btn);

    let trash_btn = gtk::Button::with_label("Move to Trash");
    trash_btn.add_css_class("flat");
    trash_btn.add_css_class("error");
    vbox.append(&trash_btn);

    if let MediaFilter::Album { ref album_id } = *filter {
        let remove_btn = gtk::Button::with_label("Remove from Album");
        remove_btn.add_css_class("flat");
        vbox.append(&remove_btn);

        let pw = pop_ref.clone();
        let sel = selection.clone();
        let tx = bus_tx.clone();
        let aid = album_id.clone();
        remove_btn.connect_clicked(move |_| {
            if let Some(p) = pw.upgrade() {
                p.popdown();
            }
            let ids = super::collect_selected_ids(&sel);
            if !ids.is_empty() {
                tx.send(AppEvent::RemoveFromAlbumRequested {
                    album_id: aid.clone(),
                    ids,
                });
            }
        });
    }

    wire_favourite_button(&fav_btn, pop_ref, selection, bus_tx, !is_favorite);
    wire_trash_button(&trash_btn, pop_ref, selection, bus_tx);
}

/// Wire the Restore button to send a restore command.
fn wire_restore_button(
    btn: &gtk::Button,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    selection: &gtk::MultiSelection,
    bus_tx: &EventSender,
) {
    let pw = pop_ref.clone();
    let sel = selection.clone();
    let tx = bus_tx.clone();
    btn.connect_clicked(move |_| {
        if let Some(p) = pw.upgrade() {
            p.popdown();
        }
        let ids = super::collect_selected_ids(&sel);
        if !ids.is_empty() {
            tx.send(AppEvent::RestoreRequested { ids });
        }
    });
}

/// Wire the Delete Permanently button with a confirmation dialog.
fn wire_permanent_delete_button(
    btn: &gtk::Button,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    selection: &gtk::MultiSelection,
    bus_tx: &EventSender,
) {
    let pw = pop_ref.clone();
    let sel = selection.clone();
    let tx = bus_tx.clone();
    btn.connect_clicked(move |btn| {
        if let Some(p) = pw.upgrade() {
            p.popdown();
        }
        let ids = super::collect_selected_ids(&sel);
        if ids.is_empty() {
            return;
        }

        let count = ids.len();
        let message = if count == 1 {
            "Permanently delete this photo? This cannot be undone.".to_string()
        } else {
            format!("Permanently delete {count} photos? This cannot be undone.")
        };

        let dialog = adw::AlertDialog::builder()
            .heading("Delete permanently?")
            .body(&message)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));

        let tx = tx.clone();
        let window = btn.root().and_downcast::<gtk::Window>();
        dialog.choose(
            window.as_ref(),
            gtk::gio::Cancellable::NONE,
            move |response| {
                if response == "delete" {
                    tx.send(AppEvent::DeleteRequested { ids });
                }
            },
        );
    });
}

/// Wire the Favourite/Unfavourite button.
fn wire_favourite_button(
    btn: &gtk::Button,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    selection: &gtk::MultiSelection,
    bus_tx: &EventSender,
    new_fav: bool,
) {
    let pw = pop_ref.clone();
    let sel = selection.clone();
    let tx = bus_tx.clone();
    btn.connect_clicked(move |_| {
        if let Some(p) = pw.upgrade() {
            p.popdown();
        }
        let ids = super::collect_selected_ids(&sel);
        if !ids.is_empty() {
            tx.send(AppEvent::FavoriteRequested {
                ids,
                state: new_fav,
            });
        }
    });
}

/// Wire the Move to Trash button.
fn wire_trash_button(
    btn: &gtk::Button,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    selection: &gtk::MultiSelection,
    bus_tx: &EventSender,
) {
    let pw = pop_ref.clone();
    let sel = selection.clone();
    let tx = bus_tx.clone();
    btn.connect_clicked(move |_| {
        if let Some(p) = pw.upgrade() {
            p.popdown();
        }
        let ids = super::collect_selected_ids(&sel);
        if !ids.is_empty() {
            tx.send(AppEvent::TrashRequested { ids });
        }
    });
}

/// Update the favourite button's icon and label to reflect the current state.
/// `all_fav = true` means all selected items are favourited → show "Unfavourite".
pub(super) fn update_fav_button(btn: &gtk::Button, all_fav: bool) {
    let Some(content) = btn.child().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(icon) = content.first_child().and_downcast::<gtk::Image>() else {
        return;
    };
    let Some(label) = icon.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };

    if all_fav {
        icon.set_icon_name(Some("non-starred-symbolic"));
        label.set_label("Unfavourite");
    } else {
        icon.set_icon_name(Some("starred-symbolic"));
        label.set_label("Favourite");
    }
}

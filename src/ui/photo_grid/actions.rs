//! Context menu and album popover wiring for the photo grid.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{gio, glib};
use tracing::debug;

use crate::library::media::{MediaFilter, MediaId};

use crate::client::MediaItemObject;

use super::selection::SelectionState;

/// Context passed to wiring functions.
///
/// Carries view references for context menus and action bar actions.
pub(super) struct ActionContext {
    pub state: SelectionState,
    pub store: gio::ListStore,
    pub filter: MediaFilter,
    pub grid_view: gtk::GridView,
}

/// Wire the "Add to Album" button to open the album picker dialog.
pub(super) fn wire_album_controls(ctx: &ActionContext, album_btn: &gtk::Button) {
    let state = ctx.state.clone();

    album_btn.connect_clicked(move |btn: &gtk::Button| {
        debug!("album button clicked");
        let ids = state.ids();
        if ids.is_empty() {
            return;
        }
        crate::ui::album_picker_dialog::show_album_picker_dialog(btn, ids);
    });
}

/// Wire single-click cell-body selection toggling.
///
/// In selection mode, clicking anywhere on a cell toggles its
/// membership in [`SelectionState`]. The gesture is on the GridView
/// itself (not each cell) because GridView wraps cells in
/// `GtkListItemWidget` which intercepts clicks before any cell-level
/// gesture fires — same reason the right-click context menu is wired
/// here too. Outside selection mode the gesture is a no-op so the
/// GridView's double-click activation continues to fire normally.
pub(super) fn wire_selection_click(ctx: &ActionContext, selection_mode: Rc<Cell<bool>>) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(1);
    // Capture-phase: GridView's internal `GtkListItemWidget` wrappers
    // handle button-1 presses on the way back up the tree (focus +
    // activation routing), so a Bubble-phase gesture never sees the
    // event. Right-click (button 3) doesn't compete and works on
    // Bubble; left-click needs Capture to win.
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);

    let grid_view = ctx.grid_view.clone();
    let state = ctx.state.clone();

    gesture.connect_pressed(move |gesture, _, x, y| {
        if !selection_mode.get() {
            return;
        }
        let Some(item) = find_clicked_item(&grid_view, x, y) else {
            return;
        };
        let id = item.item().id.clone();
        if state.contains(&id) {
            state.remove(&id);
        } else {
            state.insert(id);
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });

    ctx.grid_view.add_controller(gesture);
}

/// Wire the right-click context menu on grid cells.
///
/// Actions invoke `MediaClientV2` / `AlbumClientV2` methods directly.
pub(super) fn wire_context_menu(ctx: &ActionContext) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);

    let grid_view = ctx.grid_view.clone();
    let state = ctx.state.clone();
    let filter = ctx.filter.clone();

    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(item) = find_clicked_item(&grid_view, x, y) else {
            return;
        };
        let id = item.item().id.clone();

        // Decide the operative ids without mutating the visible
        // selection (#547 follow-up). If the clicked item is checked,
        // act on the whole selection; otherwise clear stale checks and
        // act on just this item — but don't *add* it to the selection,
        // since visible cells repaint on `state.changed` and a
        // surprise checkmark appears under the user's right-click.
        let operative_ids: Vec<MediaId> = if state.contains(&id) {
            state.ids()
        } else {
            state.clear();
            vec![id]
        };

        let is_favorite = item.is_favorite();
        let is_trash = matches!(filter, MediaFilter::Trashed);

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.set_margin_top(6);
        vbox.set_margin_bottom(6);
        vbox.set_margin_start(6);
        vbox.set_margin_end(6);

        let popover = gtk::Popover::new();
        let pop_ref: glib::WeakRef<gtk::Popover> = popover.downgrade();

        if is_trash {
            build_trash_menu(&vbox, &pop_ref, operative_ids);
        } else {
            build_standard_menu(&vbox, &pop_ref, operative_ids, &filter, is_favorite);
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

/// Walk up from the picked widget to find the bound `MediaItemObject`.
/// Correct under virtualization (positions are recycled), unlike the
/// previous position-based lookup.
fn find_clicked_item(grid_view: &gtk::GridView, x: f64, y: f64) -> Option<MediaItemObject> {
    let mut widget = grid_view.pick(x, y, gtk::PickFlags::DEFAULT);
    while let Some(ref w) = widget {
        if let Some(cell) = w.downcast_ref::<super::cell::PhotoGridCell>() {
            return cell.bound_item();
        }
        widget = w.parent();
    }
    None
}

/// Build the trash-view context menu: Restore, Delete Permanently.
fn build_trash_menu(vbox: &gtk::Box, pop_ref: &glib::WeakRef<gtk::Popover>, ids: Vec<MediaId>) {
    let restore_btn = gtk::Button::with_label(&gettext("Restore"));
    restore_btn.add_css_class("flat");
    vbox.append(&restore_btn);

    let delete_btn = gtk::Button::with_label(&gettext("Delete Permanently"));
    delete_btn.add_css_class("flat");
    delete_btn.add_css_class("error");
    vbox.append(&delete_btn);

    wire_restore_button(&restore_btn, pop_ref, ids.clone());
    wire_permanent_delete_button(&delete_btn, pop_ref, ids);
}

/// Build the standard/album context menu: Favourite, Move to Trash,
/// and optionally Remove from Album.
fn build_standard_menu(
    vbox: &gtk::Box,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    ids: Vec<MediaId>,
    filter: &MediaFilter,
    is_favorite: bool,
) {
    let fav_label = if is_favorite {
        gettext("Unfavourite")
    } else {
        gettext("Favourite")
    };
    let fav_btn = gtk::Button::with_label(&fav_label);
    fav_btn.add_css_class("flat");
    vbox.append(&fav_btn);

    let trash_btn = gtk::Button::with_label(&gettext("Move to Trash"));
    trash_btn.add_css_class("flat");
    trash_btn.add_css_class("error");
    vbox.append(&trash_btn);

    if let MediaFilter::Album { ref album_id } = *filter {
        let remove_btn = gtk::Button::with_label(&gettext("Remove from Album"));
        remove_btn.add_css_class("flat");
        vbox.append(&remove_btn);

        let pw = pop_ref.clone();
        let aid = album_id.clone();
        let ids_for_remove = ids.clone();
        remove_btn.connect_clicked(move |_| {
            if let Some(p) = pw.upgrade() {
                p.popdown();
            }
            if ids_for_remove.is_empty() {
                return;
            }
            crate::application::MomentsApplication::default()
                .album_client_v2()
                .remove_from_album(aid.clone(), ids_for_remove.clone());
        });
    }

    wire_favourite_button(&fav_btn, pop_ref, ids.clone(), !is_favorite);
    wire_trash_button(&trash_btn, pop_ref, ids);
}

/// Wire the Restore button to send a restore command.
fn wire_restore_button(
    btn: &gtk::Button,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    ids: Vec<MediaId>,
) {
    let pw = pop_ref.clone();
    btn.connect_clicked(move |_| {
        if let Some(p) = pw.upgrade() {
            p.popdown();
        }
        if ids.is_empty() {
            return;
        }
        crate::application::MomentsApplication::default()
            .media_client_v2()
            .restore(ids.clone());
    });
}

/// Wire the Delete Permanently button with a confirmation dialog.
fn wire_permanent_delete_button(
    btn: &gtk::Button,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    ids: Vec<MediaId>,
) {
    let pw = pop_ref.clone();
    btn.connect_clicked(move |btn| {
        if let Some(p) = pw.upgrade() {
            p.popdown();
        }
        if ids.is_empty() {
            return;
        }

        let count = ids.len();
        let message = ngettext(
            "Permanently delete this photo? This cannot be undone.",
            "Permanently delete {} photos? This cannot be undone.",
            count as u32,
        )
        .replace("{}", &count.to_string());

        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Delete permanently?"))
            .body(&message)
            .build();
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("delete", &gettext("Delete"));
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));

        let window = btn.root().and_downcast::<gtk::Window>();
        let ids_for_dialog = ids.clone();
        dialog.choose(
            window.as_ref(),
            gtk::gio::Cancellable::NONE,
            move |response| {
                if response == "delete" {
                    crate::application::MomentsApplication::default()
                        .media_client_v2()
                        .delete(ids_for_dialog);
                }
            },
        );
    });
}

/// Wire the Favourite/Unfavourite button.
fn wire_favourite_button(
    btn: &gtk::Button,
    pop_ref: &glib::WeakRef<gtk::Popover>,
    ids: Vec<MediaId>,
    new_fav: bool,
) {
    let pw = pop_ref.clone();
    btn.connect_clicked(move |_| {
        if let Some(p) = pw.upgrade() {
            p.popdown();
        }
        if ids.is_empty() {
            return;
        }
        crate::application::MomentsApplication::default()
            .media_client_v2()
            .set_favorite(ids.clone(), new_fav);
    });
}

/// Wire the Move to Trash button.
fn wire_trash_button(btn: &gtk::Button, pop_ref: &glib::WeakRef<gtk::Popover>, ids: Vec<MediaId>) {
    let pw = pop_ref.clone();
    btn.connect_clicked(move |_| {
        if let Some(p) = pw.upgrade() {
            p.popdown();
        }
        if ids.is_empty() {
            return;
        }
        crate::application::MomentsApplication::default()
            .media_client_v2()
            .trash(ids.clone());
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
        label.set_label(&gettext("Unfavourite"));
    } else {
        icon.set_icon_name(Some("starred-symbolic"));
        label.set_label(&gettext("Favourite"));
    }
}

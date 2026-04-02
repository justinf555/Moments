//! Factory for building context-sensitive action bar buttons.
//!
//! The action bar buttons change depending on which view the user is in:
//! - **Standard** (Photos, Favourites, Recent, People): Favourite, Add to album, Delete
//! - **Trash**: Restore, Delete permanently
//! - **Album**: Favourite, Remove from album, Delete
//!
//! Button handlers emit command events via the [`EventBus`] — they resolve
//! UI state (selection → IDs) and nothing else. The
//! [`CommandDispatcher`](crate::commands::dispatcher::CommandDispatcher)
//! handles execution and emits result events.

use adw::prelude::*;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::album::AlbumId;
use crate::library::media::MediaFilter;

use super::actions;
use super::item::MediaItemObject;

/// The built action bar buttons and the container box.
pub struct ActionBarButtons {
    /// The horizontal box containing all buttons — set as centre widget on the ActionBar.
    pub container: gtk::Box,
    /// The favourite/unfavourite button (if present). Stored for dynamic label updates.
    pub fav_btn: Option<gtk::Button>,
    /// The "Add to album" button (if present). Needs separate wiring via
    /// `wire_album_controls` since it requires library queries for the popover.
    pub album_btn: Option<gtk::Button>,
}

/// Build action bar buttons appropriate for the given filter.
///
/// Returns wired buttons ready to be placed in a `gtk::ActionBar`.
pub fn build_for_filter(
    filter: &MediaFilter,
    selection: &gtk::MultiSelection,
    bus: &EventSender,
) -> ActionBarButtons {
    match filter {
        MediaFilter::Trashed => build_trash_bar(selection, bus),
        MediaFilter::Album { album_id } => build_album_bar(selection, bus, album_id),
        _ => build_standard_bar(selection, bus),
    }
}

// ── Standard: Favourite, Add to album, Delete ────────────────────────────────

fn build_standard_bar(
    selection: &gtk::MultiSelection,
    bus: &EventSender,
) -> ActionBarButtons {
    let fav_btn = make_button("starred-symbolic", "Favourite");
    fav_btn.set_width_request(150);
    let album_btn = make_button("folder-new-symbolic", "Add to album");
    let trash_btn = make_button("user-trash-symbolic", "Delete");

    wire_favourite(&fav_btn, selection, bus);
    wire_trash(&trash_btn, selection, bus);

    let container = bar_container();
    container.append(&fav_btn);
    container.append(&album_btn);
    container.append(&trash_btn);

    ActionBarButtons {
        container,
        fav_btn: Some(fav_btn),
        album_btn: Some(album_btn),
    }
}

// ── Trash: Restore, Delete permanently ───────────────────────────────────────

fn build_trash_bar(
    selection: &gtk::MultiSelection,
    bus: &EventSender,
) -> ActionBarButtons {
    let restore_btn = make_button("edit-undo-symbolic", "Restore");
    let delete_btn = make_button("edit-delete-symbolic", "Delete permanently");

    wire_restore(&restore_btn, selection, bus);
    wire_delete_permanently(&delete_btn, selection, bus);

    let container = bar_container();
    container.append(&restore_btn);
    container.append(&delete_btn);

    ActionBarButtons {
        container,
        fav_btn: None,
        album_btn: None,
    }
}

// ── Album: Favourite, Remove from album, Delete ──────────────────────────────

fn build_album_bar(
    selection: &gtk::MultiSelection,
    bus: &EventSender,
    album_id: &AlbumId,
) -> ActionBarButtons {
    let fav_btn = make_button("starred-symbolic", "Favourite");
    fav_btn.set_width_request(150);
    let remove_btn = make_button("list-remove-symbolic", "Remove from album");
    let trash_btn = make_button("user-trash-symbolic", "Delete");

    wire_favourite(&fav_btn, selection, bus);
    wire_remove_from_album(&remove_btn, selection, bus, album_id);
    wire_trash(&trash_btn, selection, bus);

    let container = bar_container();
    container.append(&fav_btn);
    container.append(&remove_btn);
    container.append(&trash_btn);

    ActionBarButtons {
        container,
        fav_btn: Some(fav_btn),
        album_btn: None,
    }
}

// ── Button construction ──────────────────────────────────────────────────────

fn make_button(icon_name: &str, label: &str) -> gtk::Button {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::Center)
        .build();
    content.append(&gtk::Image::from_icon_name(icon_name));
    content.append(&gtk::Label::new(Some(label)));

    let btn = gtk::Button::builder()
        .child(&content)
        .build();
    btn.add_css_class("outlined");
    btn
}

fn bar_container() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(24)
        .halign(gtk::Align::Center)
        .build()
}

// ── Wiring ───────────────────────────────────────────────────────────────────

fn wire_favourite(
    btn: &gtk::Button,
    selection: &gtk::MultiSelection,
    bus: &EventSender,
) {
    let sel = selection.clone();
    let tx = bus.clone();
    let btn_ref = btn.clone();
    btn.connect_clicked(move |_| {
        let ids = super::collect_selected_ids(&sel);
        if ids.is_empty() { return; }

        let first_fav = sel.item(sel.selection().nth(0))
            .and_then(|o| o.downcast::<MediaItemObject>().ok())
            .map(|o| o.is_favorite())
            .unwrap_or(false);
        let new_state = !first_fav;

        tx.send(AppEvent::FavoriteRequested { ids, state: new_state });
        actions::update_fav_button(&btn_ref, new_state);
    });
}

fn wire_trash(
    btn: &gtk::Button,
    selection: &gtk::MultiSelection,
    bus: &EventSender,
) {
    let sel = selection.clone();
    let tx = bus.clone();
    btn.connect_clicked(move |_| {
        let ids = super::collect_selected_ids(&sel);
        if !ids.is_empty() {
            tx.send(AppEvent::TrashRequested { ids });
        }
    });
}

fn wire_restore(
    btn: &gtk::Button,
    selection: &gtk::MultiSelection,
    bus: &EventSender,
) {
    let sel = selection.clone();
    let tx = bus.clone();
    btn.connect_clicked(move |_| {
        let ids = super::collect_selected_ids(&sel);
        if !ids.is_empty() {
            tx.send(AppEvent::RestoreRequested { ids });
        }
    });
}

fn wire_delete_permanently(
    btn: &gtk::Button,
    selection: &gtk::MultiSelection,
    bus: &EventSender,
) {
    let sel = selection.clone();
    let tx = bus.clone();
    btn.connect_clicked(move |btn| {
        let ids = super::collect_selected_ids(&sel);
        if ids.is_empty() { return; }

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
        dialog.choose(window.as_ref(), gtk::gio::Cancellable::NONE, move |response| {
            if response == "delete" {
                tx.send(AppEvent::DeleteRequested { ids });
            }
        });
    });
}

fn wire_remove_from_album(
    btn: &gtk::Button,
    selection: &gtk::MultiSelection,
    bus: &EventSender,
    album_id: &AlbumId,
) {
    let sel = selection.clone();
    let tx = bus.clone();
    let aid = album_id.clone();
    btn.connect_clicked(move |_| {
        let ids = super::collect_selected_ids(&sel);
        if !ids.is_empty() {
            tx.send(AppEvent::RemoveFromAlbumRequested {
                album_id: aid.clone(),
                ids,
            });
        }
    });
}

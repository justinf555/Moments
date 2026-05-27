//! Factory for building context-sensitive action bar buttons.
//!
//! The action bar buttons change depending on which view the user is in:
//! - **Standard** (Photos, Favourites, Recent, People): Favourite, Add to album, Delete
//! - **Trash**: Restore, Delete permanently
//! - **Album**: Favourite, Remove from album, Delete

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::gio;

use crate::library::album::AlbumId;
use crate::library::media::MediaFilter;

use super::actions;
use super::selection::SelectionState;

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
    state: &SelectionState,
    store: &gio::ListStore,
) -> ActionBarButtons {
    match filter {
        MediaFilter::Trashed => build_trash_bar(state),
        MediaFilter::Album { album_id } => build_album_bar(state, store, album_id),
        _ => build_standard_bar(state, store),
    }
}

// ── Standard: Favourite, Add to album, Delete ────────────────────────────────

fn build_standard_bar(state: &SelectionState, store: &gio::ListStore) -> ActionBarButtons {
    let fav_btn = make_button("starred-symbolic", &gettext("Favourite"));
    fav_btn.set_width_request(150);
    let album_btn = make_button("folder-new-symbolic", &gettext("Add to album"));
    let trash_btn = make_button("user-trash-symbolic", &gettext("Delete"));

    wire_favourite(&fav_btn, state, store);
    wire_trash(&trash_btn, state);

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

fn build_trash_bar(state: &SelectionState) -> ActionBarButtons {
    let restore_btn = make_button("edit-undo-symbolic", &gettext("Restore"));
    let delete_btn = make_button("edit-delete-symbolic", &gettext("Delete permanently"));

    wire_restore(&restore_btn, state);
    wire_delete_permanently(&delete_btn, state);

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
    state: &SelectionState,
    store: &gio::ListStore,
    album_id: &AlbumId,
) -> ActionBarButtons {
    let fav_btn = make_button("starred-symbolic", &gettext("Favourite"));
    fav_btn.set_width_request(150);
    let remove_btn = make_button("list-remove-symbolic", &gettext("Remove from album"));
    let trash_btn = make_button("user-trash-symbolic", &gettext("Delete"));

    wire_favourite(&fav_btn, state, store);
    wire_remove_from_album(&remove_btn, state, album_id);
    wire_trash(&trash_btn, state);

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

    let btn = gtk::Button::builder().child(&content).build();
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

fn wire_favourite(btn: &gtk::Button, state: &SelectionState, store: &gio::ListStore) {
    let s = state.clone();
    let st = store.clone();
    let btn_ref = btn.clone();
    btn.connect_clicked(move |_| {
        let ids = s.ids();
        if ids.is_empty() {
            return;
        }

        // Toggle direction matches the visible button label, which is
        // derived from `all_fav` in the selection-changed handler:
        //   all favourited → label "Unfavourite" → click unfavourites all
        //   any unfavourited → label "Favourite"  → click favourites all
        // This is order-independent, unlike `s.ids().first()` which
        // would depend on HashSet iteration order.
        let all_fav = ids.iter().all(|id| {
            super::find_item_in_store(&st, id)
                .map(|o| o.is_favorite())
                .unwrap_or(false)
        });
        let new_state = !all_fav;

        crate::application::MomentsApplication::default()
            .media_client_v2()
            .set_favorite(ids, new_state);
        actions::update_fav_button(&btn_ref, new_state);
    });
}

fn wire_trash(btn: &gtk::Button, state: &SelectionState) {
    let s = state.clone();
    btn.connect_clicked(move |_| {
        let ids = s.ids();
        if ids.is_empty() {
            return;
        }
        crate::application::MomentsApplication::default()
            .media_client_v2()
            .trash(ids);
    });
}

fn wire_restore(btn: &gtk::Button, state: &SelectionState) {
    let s = state.clone();
    btn.connect_clicked(move |_| {
        let ids = s.ids();
        if ids.is_empty() {
            return;
        }
        crate::application::MomentsApplication::default()
            .media_client_v2()
            .restore(ids);
    });
}

fn wire_delete_permanently(btn: &gtk::Button, state: &SelectionState) {
    let s = state.clone();
    btn.connect_clicked(move |btn| {
        let ids = s.ids();
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
        dialog.choose(
            window.as_ref(),
            gtk::gio::Cancellable::NONE,
            move |response| {
                if response == "delete" {
                    crate::application::MomentsApplication::default()
                        .media_client_v2()
                        .delete(ids);
                }
            },
        );
    });
}

fn wire_remove_from_album(btn: &gtk::Button, state: &SelectionState, album_id: &AlbumId) {
    let s = state.clone();
    let aid = album_id.clone();
    btn.connect_clicked(move |_| {
        let ids = s.ids();
        if ids.is_empty() {
            return;
        }
        crate::application::MomentsApplication::default()
            .album_client_v2()
            .remove_from_album(aid.clone(), ids);
    });
}

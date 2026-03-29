//! Factory for building context-sensitive action bar buttons.
//!
//! The action bar buttons change depending on which view the user is in:
//! - **Standard** (Photos, Favourites, Recent, People): Favourite, Add to album, Delete
//! - **Trash**: Restore, Delete permanently
//! - **Album**: Favourite, Remove from album, Delete

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::library::album::AlbumId;
use crate::library::media::MediaFilter;
use crate::library::Library;
use crate::ui::model_registry::ModelRegistry;

use super::actions;
use super::item::MediaItemObject;

/// The built action bar buttons and the container box.
pub struct ActionBarButtons {
    /// The horizontal box containing all buttons — set as centre widget on the ActionBar.
    pub container: gtk::Box,
    /// The favourite/unfavourite button (if present). Stored for dynamic label updates.
    pub fav_btn: Option<gtk::Button>,
}

/// Build action bar buttons appropriate for the given filter.
///
/// Returns wired buttons ready to be placed in a `gtk::ActionBar`.
pub fn build_for_filter(
    filter: &MediaFilter,
    selection: &gtk::MultiSelection,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    registry: &Rc<ModelRegistry>,
    exit_selection: &gio::SimpleAction,
) -> ActionBarButtons {
    match filter {
        MediaFilter::Trashed => build_trash_bar(selection, library, tokio, registry, exit_selection),
        MediaFilter::Album { album_id } => {
            build_album_bar(selection, library, tokio, registry, album_id, exit_selection)
        }
        _ => build_standard_bar(selection, library, tokio, registry, exit_selection),
    }
}

// ── Standard: Favourite, Add to album, Delete ────────────────────────────────

fn build_standard_bar(
    selection: &gtk::MultiSelection,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    registry: &Rc<ModelRegistry>,
    exit_selection: &gio::SimpleAction,
) -> ActionBarButtons {
    let fav_btn = make_button("starred-symbolic", "Favourite");
    fav_btn.set_width_request(150);
    let album_btn = make_button("folder-new-symbolic", "Add to album");
    let trash_btn = make_button("user-trash-symbolic", "Delete");

    wire_favourite(&fav_btn, selection, library, tokio, registry);
    wire_trash(&trash_btn, selection, library, tokio, registry, exit_selection);

    let container = bar_container();
    container.append(&fav_btn);
    container.append(&album_btn);
    container.append(&trash_btn);

    ActionBarButtons {
        container,
        fav_btn: Some(fav_btn),
    }
}

// ── Trash: Restore, Delete permanently ───────────────────────────────────────

fn build_trash_bar(
    selection: &gtk::MultiSelection,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    registry: &Rc<ModelRegistry>,
    exit_selection: &gio::SimpleAction,
) -> ActionBarButtons {
    let restore_btn = make_button("edit-undo-symbolic", "Restore");
    let delete_btn = make_button("edit-delete-symbolic", "Delete permanently");

    wire_restore(&restore_btn, selection, library, tokio, registry, exit_selection);
    wire_delete_permanently(&delete_btn, selection, library, tokio, registry, exit_selection);

    let container = bar_container();
    container.append(&restore_btn);
    container.append(&delete_btn);

    ActionBarButtons {
        container,
        fav_btn: None,
    }
}

// ── Album: Favourite, Remove from album, Delete ──────────────────────────────

fn build_album_bar(
    selection: &gtk::MultiSelection,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    registry: &Rc<ModelRegistry>,
    album_id: &AlbumId,
    exit_selection: &gio::SimpleAction,
) -> ActionBarButtons {
    let fav_btn = make_button("starred-symbolic", "Favourite");
    fav_btn.set_width_request(150);
    let remove_btn = make_button("list-remove-symbolic", "Remove from album");
    let trash_btn = make_button("user-trash-symbolic", "Delete");

    wire_favourite(&fav_btn, selection, library, tokio, registry);
    wire_remove_from_album(&remove_btn, selection, library, tokio, registry, album_id, exit_selection);
    wire_trash(&trash_btn, selection, library, tokio, registry, exit_selection);

    let container = bar_container();
    container.append(&fav_btn);
    container.append(&remove_btn);
    container.append(&trash_btn);

    ActionBarButtons {
        container,
        fav_btn: Some(fav_btn),
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
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    registry: &Rc<ModelRegistry>,
) {
    let sel = selection.clone();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let reg = Rc::clone(registry);
    let btn_ref = btn.clone();
    btn.connect_clicked(move |_| {
        let ids = super::collect_selected_ids(&sel);
        if ids.is_empty() { return; }

        let first_fav = sel.item(sel.selection().nth(0))
            .and_then(|o| o.downcast::<MediaItemObject>().ok())
            .map(|o| o.is_favorite())
            .unwrap_or(false);
        let new_state = !first_fav;

        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let reg = Rc::clone(&reg);
        let id_list = ids.clone();
        let btn = btn_ref.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tk
                .spawn(async move { lib.set_favorite(&id_list, new_state).await })
                .await;
            if let Ok(Ok(())) = result {
                for id in &ids {
                    reg.on_favorite_changed(id, new_state);
                }
                actions::update_fav_button(&btn, new_state);
            }
        });
    });
}

fn wire_trash(
    btn: &gtk::Button,
    selection: &gtk::MultiSelection,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    registry: &Rc<ModelRegistry>,
    exit_selection: &gio::SimpleAction,
) {
    let sel = selection.clone();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let reg = Rc::clone(registry);
    let exit = exit_selection.clone();
    btn.connect_clicked(move |_| {
        let ids = super::collect_selected_ids(&sel);
        if ids.is_empty() { return; }

        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let reg = Rc::clone(&reg);
        let exit = exit.clone();
        let ids_for_action = ids.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tk
                .spawn(async move { lib.trash(&ids_for_action).await })
                .await;
            if let Ok(Ok(())) = result {
                for id in &ids {
                    reg.on_trashed(&id, true);
                }
                exit.activate(None);
            }
        });
    });
}

fn wire_restore(
    btn: &gtk::Button,
    selection: &gtk::MultiSelection,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    registry: &Rc<ModelRegistry>,
    exit_selection: &gio::SimpleAction,
) {
    let sel = selection.clone();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let reg = Rc::clone(registry);
    let exit = exit_selection.clone();
    btn.connect_clicked(move |_| {
        let ids = super::collect_selected_ids(&sel);
        if ids.is_empty() { return; }

        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let reg = Rc::clone(&reg);
        let exit = exit.clone();
        let ids_for_action = ids.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tk
                .spawn(async move { lib.restore(&ids_for_action).await })
                .await;
            if let Ok(Ok(())) = result {
                for id in &ids {
                    reg.on_deleted(id);
                }
                exit.activate(None);
            }
        });
    });
}

fn wire_delete_permanently(
    btn: &gtk::Button,
    selection: &gtk::MultiSelection,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    registry: &Rc<ModelRegistry>,
    exit_selection: &gio::SimpleAction,
) {
    let sel = selection.clone();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let reg = Rc::clone(registry);
    let exit_selection = exit_selection.clone();
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

        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let reg = Rc::clone(&reg);
        let exit = exit_selection.clone();
        let window = btn.root().and_downcast::<gtk::Window>();
        dialog.choose(window.as_ref(), gtk::gio::Cancellable::NONE, move |response| {
            if response != "delete" { return; }
            let lib = Arc::clone(&lib);
            let tk = tk.clone();
            let reg = Rc::clone(&reg);
            let exit = exit.clone();
            let ids_for_action = ids.clone();
            glib::MainContext::default().spawn_local(async move {
                let result = tk
                    .spawn(async move { lib.delete_permanently(&ids_for_action).await })
                    .await;
                if let Ok(Ok(())) = result {
                    for id in &ids {
                        reg.on_deleted(id);
                    }
                    exit.activate(None);
                }
            });
        });
    });
}

fn wire_remove_from_album(
    btn: &gtk::Button,
    selection: &gtk::MultiSelection,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    registry: &Rc<ModelRegistry>,
    album_id: &AlbumId,
    exit_selection: &gio::SimpleAction,
) {
    let sel = selection.clone();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let reg = Rc::clone(registry);
    let aid = album_id.clone();
    let exit = exit_selection.clone();
    btn.connect_clicked(move |_| {
        let ids = super::collect_selected_ids(&sel);
        if ids.is_empty() { return; }

        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let reg = Rc::clone(&reg);
        let aid = aid.clone();
        let aid_log = aid.clone();
        let exit = exit.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tk
                .spawn(async move { lib.remove_from_album(&aid, &ids).await })
                .await;
            if let Ok(Ok(())) = result {
                reg.on_album_media_changed(&aid_log);
                exit.activate(None);
            }
        });
    });
}

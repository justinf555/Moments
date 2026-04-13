use adw::prelude::*;

/// Show a dialog for creating a new album.
///
/// Presents an `AdwAlertDialog` with a text entry. Calls `on_create` with
/// the entered name when the user confirms. The "Create" button is disabled
/// while the entry is empty.
pub fn show_create_album_dialog(
    window: &impl IsA<gtk::Widget>,
    on_create: impl Fn(String) + 'static,
) {
    let dialog = adw::AlertDialog::new(Some("New Album"), None);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("create", "Create");
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("create"));
    dialog.set_close_response("cancel");

    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("Album name"));
    entry.set_margin_top(12);
    entry.set_margin_start(12);
    entry.set_margin_end(12);
    entry.set_activates_default(true);

    // Disable "Create" while entry is empty.
    dialog.set_response_enabled("create", false);
    let dialog_weak = dialog.downgrade();
    entry.connect_changed(move |e| {
        if let Some(d) = dialog_weak.upgrade() {
            d.set_response_enabled("create", !e.text().is_empty());
        }
    });

    dialog.set_extra_child(Some(&entry));

    let entry_ref = entry.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "create" {
            let name = entry_ref.text().to_string();
            if !name.is_empty() {
                on_create(name);
            }
        }
    });

    dialog.present(Some(window));
}

/// Show a dialog for renaming an album.
///
/// Pre-fills the entry with `current_name`. Calls `on_rename` with the
/// new name when the user confirms.
pub fn show_rename_album_dialog(
    window: &impl IsA<gtk::Widget>,
    current_name: &str,
    on_rename: impl Fn(String) + 'static,
) {
    let dialog = adw::AlertDialog::new(Some("Rename Album"), None);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("rename", "Rename");
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("rename"));
    dialog.set_close_response("cancel");

    let entry = gtk::Entry::new();
    entry.set_text(current_name);
    entry.set_margin_top(12);
    entry.set_margin_start(12);
    entry.set_margin_end(12);
    entry.set_activates_default(true);
    // Select all text so the user can immediately type a replacement.
    entry.select_region(0, -1);

    let dialog_weak = dialog.downgrade();
    entry.connect_changed(move |e| {
        if let Some(d) = dialog_weak.upgrade() {
            d.set_response_enabled("rename", !e.text().is_empty());
        }
    });

    dialog.set_extra_child(Some(&entry));

    let entry_ref = entry.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "rename" {
            let name = entry_ref.text().to_string();
            if !name.is_empty() {
                on_rename(name);
            }
        }
    });

    dialog.present(Some(window));
}

/// Show a confirmation dialog for deleting an album.
///
/// The dialog warns that the album will be deleted but photos remain in the
/// library. Calls `on_delete` when the user confirms.
pub fn show_delete_album_dialog(
    window: &impl IsA<gtk::Widget>,
    album_name: &str,
    on_delete: impl Fn() + 'static,
) {
    let body = format!(
        "The album \u{201c}{album_name}\u{201d} will be deleted. \
         Photos in this album will not be removed from your library."
    );
    let dialog = adw::AlertDialog::new(Some("Delete Album?"), Some(&body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            on_delete();
        }
    });

    dialog.present(Some(window));
}

// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::prelude::*;

/// Present the keyboard shortcuts dialog over `parent`.
///
/// The dialog is a plain `AdwShortcutsDialog` defined in
/// `shortcuts_dialog.blp`; there is no custom widget subclass.
pub fn show_shortcuts(parent: &impl IsA<gtk::Widget>) {
    let builder = gtk::Builder::from_resource(
        "/io/github/justinf555/Moments/ui/shortcuts_dialog/shortcuts_dialog.ui",
    );
    let dialog = builder
        .object::<adw::ShortcutsDialog>("shortcuts_dialog")
        .expect("shortcuts_dialog in resource");
    dialog.present(Some(parent));
}

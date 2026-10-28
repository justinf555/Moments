// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Folder import: the folder picker. Scanning and importing live in
//! [`crate::client::ImportClient`].

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::{error, warn};

use super::MomentsApplication;

impl MomentsApplication {
    /// Open a folder picker and start importing the selected folder.
    pub(super) fn show_import_dialog(&self) {
        let window = match self.active_window() {
            Some(w) => w,
            None => return,
        };

        let file_dialog = gtk::FileDialog::builder()
            .title(gettext("Select Folder to Import"))
            .modal(true)
            .build();

        file_dialog.select_folder(
            Some(&window),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |result| if let Ok(folder) = result {
                    app.run_import(folder);
                }
            ),
        );
    }

    /// Hand the chosen folder to the import client, which scans and
    /// imports it. Progress shows in the sidebar's activity indicator.
    ///
    /// Passes on the `gio::File` from the file dialog rather than a path.
    /// This is critical for Flatpak: the document portal grants access to
    /// the `gio::File` object, but the underlying path (`/run/user/…/doc/…`)
    /// is only reachable through it. `ImportClient::import_folder` walks
    /// it with GIO for that reason.
    fn run_import(&self, folder: gio::File) {
        let Some(import_client) = self.import_client() else {
            error!("import requested but no library is open");
            return;
        };
        if import_client.is_busy() {
            warn!("import already in progress");
            return;
        }
        import_client.import_folder(folder);
    }
}

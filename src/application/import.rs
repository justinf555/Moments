/* application/import.rs
 *
 * Copyright 2026 Unknown
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Folder-import entry points: the folder picker dialog and the
//! pipeline kickoff, plus the GIO helpers that preserve Flatpak portal
//! grants while enumerating the selected folder.

use std::path::PathBuf;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use tracing::{debug, error, info, warn};

use super::MomentsApplication;

impl MomentsApplication {
    /// Open a folder picker and start importing the selected folder.
    pub(in crate::application) fn show_import_dialog(&self) {
        let window = match self.active_window() {
            Some(w) => w,
            None => return,
        };

        let file_dialog = gtk::FileDialog::builder()
            .title("Select Folder to Import")
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

    /// Create the import progress dialog and kick off the import pipeline.
    ///
    /// Accepts the `gio::File` directly from the file dialog rather than
    /// extracting a path. This is critical for Flatpak: the document portal
    /// grants access to the `gio::File` object, but the underlying path
    /// (`/run/user/…/doc/…`) becomes inaccessible once the dialog callback
    /// returns. Using `gio::File::enumerate_children` on the original object
    /// respects the portal grant.
    fn run_import(&self, folder: gio::File) {
        let Some(import_client) = self.imp().import_client.get() else {
            error!("import requested but no library is open");
            return;
        };

        let display_path = folder
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| folder.uri().to_string());
        info!(path = %display_path, "starting import");

        // Resolve folder contents via GIO to handle Flatpak portal paths.
        let sources = resolve_folder_via_gio(&folder);
        if sources.is_empty() {
            warn!(path = %display_path, "no files found in folder");
            return;
        }
        debug!(count = sources.len(), "resolved import sources via GIO");

        import_client.import(sources);
    }
}

/// Recursively enumerate a folder's contents using GIO.
///
/// Accepts the `gio::File` directly from the file dialog so that
/// Flatpak document portal grants are preserved. Creating a new
/// `gio::File::for_path` from the extracted path would lose the grant.
fn resolve_folder_via_gio(folder: &gio::File) -> Vec<PathBuf> {
    let mut files = Vec::new();
    gio_walk(folder, &mut files);
    files
}

fn gio_walk(dir: &gio::File, out: &mut Vec<PathBuf>) {
    let enumerator = match dir.enumerate_children(
        "standard::name,standard::type",
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    ) {
        Ok(e) => e,
        Err(e) => {
            warn!(path = ?dir.path(), error = %e, "could not enumerate directory via GIO");
            return;
        }
    };

    while let Some(info) = enumerator.next_file(gio::Cancellable::NONE).ok().flatten() {
        let child = enumerator.child(&info);
        if info.file_type() == gio::FileType::Directory {
            gio_walk(&child, out);
        } else if let Some(path) = child.path() {
            out.push(path);
        }
    }
}

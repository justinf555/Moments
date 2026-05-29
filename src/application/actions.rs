/* application/actions.rs
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

//! App-level GAction wiring and the dialogs they present (about,
//! shortcuts, preferences). Pure UI glue — no orchestration logic.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::gio;

use super::MomentsApplication;
use crate::config::{APP_ID, PROFILE, VERSION};

impl MomentsApplication {
    pub(in crate::application) fn setup_gactions(&self) {
        let quit_action = gio::ActionEntry::builder("quit")
            .activate(move |app: &Self, _, _| app.quit())
            .build();
        let about_action = gio::ActionEntry::builder("about")
            .activate(move |app: &Self, _, _| app.show_about())
            .build();
        let import_action = gio::ActionEntry::builder("import")
            .activate(move |app: &Self, _, _| app.show_import_dialog())
            .build();
        let preferences_action = gio::ActionEntry::builder("preferences")
            .activate(move |app: &Self, _, _| app.show_preferences())
            .build();
        let shortcuts_action = gio::ActionEntry::builder("shortcuts")
            .activate(move |app: &Self, _, _| app.show_shortcuts())
            .build();
        self.add_action_entries([
            quit_action,
            about_action,
            import_action,
            preferences_action,
            shortcuts_action,
        ]);
    }

    fn show_shortcuts(&self) {
        let Some(window) = self.active_window() else {
            return;
        };
        let builder =
            gtk::Builder::from_resource("/io/github/justinf555/Moments/shortcuts-dialog.ui");
        let dialog = builder
            .object::<adw::ShortcutsDialog>("shortcuts_dialog")
            .expect("shortcuts_dialog in resource");
        dialog.present(Some(&window));
    }

    fn show_about(&self) {
        let Some(window) = self.active_window() else {
            return;
        };
        let app_name = if PROFILE == "development" {
            "Moments (Development)"
        } else {
            "Moments"
        };
        let about = adw::AboutDialog::builder()
            .application_name(app_name)
            .application_icon(APP_ID)
            .developer_name("Unknown")
            .version(VERSION)
            .developers(vec!["Unknown"])
            .translator_credits(gettext("translator-credits"))
            .copyright("© 2026 Unknown")
            .build();

        about.present(Some(&window));
    }

    fn show_preferences(&self) {
        let window = match self.active_window() {
            Some(w) => w,
            None => return,
        };
        let settings = self
            .imp()
            .settings
            .get()
            .expect("settings initialised")
            .clone();
        let is_immich = self.imp().is_immich.get();
        let immich_url = self.imp().immich_server_url.borrow().clone();

        crate::ui::preferences_dialog::show_preferences(&window, &settings, is_immich, immich_url);
    }
}

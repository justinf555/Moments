// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! App-level GActions and the dialogs they open (about, preferences,
//! shortcuts).

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::gio;

use crate::config::{APP_ID, APP_NAME, VERSION};

use super::MomentsApplication;

impl MomentsApplication {
    pub(super) fn setup_gactions(&self) {
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
        crate::ui::shortcuts_dialog::show_shortcuts(&window);
    }

    fn show_about(&self) {
        let Some(window) = self.active_window() else {
            return;
        };
        let about = adw::AboutDialog::builder()
            .application_name(APP_NAME)
            .application_icon(APP_ID)
            .developer_name("Justin F")
            .version(VERSION)
            .developers(vec!["Justin F"])
            .translator_credits(gettext("translator-credits"))
            .copyright("© 2026 Justin F")
            .build();

        about.present(Some(&window));
    }

    fn show_preferences(&self) {
        let window = match self.active_window() {
            Some(w) => w,
            None => return,
        };
        let settings = self.settings();
        let is_immich = self.imp().is_immich.get();
        let immich_url = self.imp().immich_server_url.borrow().clone();

        crate::ui::preferences_dialog::show_preferences(&window, &settings, is_immich, immich_url);
    }
}

pub mod album;
pub mod import_client;
pub mod media;
pub mod people;

pub use album::{AlbumClient, AlbumClientV2, AlbumItemObject};
pub use import_client::{ImportClient, ImportState};
pub use media::{MediaClient, MediaItemObject};
pub use people::{PeopleClient, PersonItemObject};

use gtk::glib;
use gtk::prelude::*;

/// Show a toast via the `win.show-toast` action.
///
/// Safe to call from any thread — defers to the GTK main loop.
pub fn show_toast(message: &str) {
    let msg = message.to_string();
    glib::idle_add_once(move || {
        let app = crate::application::MomentsApplication::default();
        if let Some(window) = app.active_window() {
            WidgetExt::activate_action(&window, "win.show-toast", Some(&msg.to_variant())).ok();
        }
    });
}

use adw::prelude::*;
use gtk;

use super::ContentView;

/// Full-screen status page shown when the library has no photos.
///
/// Registered as the "empty" route in the `ContentCoordinator`. The
/// "Import Photos" button fires `app.import` — no extra wiring needed.
pub struct EmptyLibraryView {
    widget: gtk::Widget,
}

impl EmptyLibraryView {
    pub fn new() -> Self {
        let page = adw::StatusPage::builder()
            .icon_name("camera-photo-symbolic")
            .title("No Photos Yet")
            .description("Import photos or wait for sync to populate your library.")
            .vexpand(true)
            .build();

        let widget = page.upcast::<gtk::Widget>();
        Self { widget }
    }
}

impl ContentView for EmptyLibraryView {
    fn widget(&self) -> &gtk::Widget {
        &self.widget
    }
}

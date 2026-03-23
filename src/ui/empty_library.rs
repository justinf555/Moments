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
        let import_btn = gtk::Button::builder()
            .label("Import Photos\u{2026}")
            .halign(gtk::Align::Center)
            .action_name("app.import")
            .build();
        import_btn.add_css_class("suggested-action");
        import_btn.add_css_class("pill");

        let page = adw::StatusPage::builder()
            .icon_name("camera-photo-symbolic")
            .title("No Photos Yet")
            .description("Import a folder of photos to get started.")
            .vexpand(true)
            .child(&import_btn)
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

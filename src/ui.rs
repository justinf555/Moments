use gtk;

pub mod album_dialogs;
pub mod coordinator;
pub mod empty_library;
pub mod import_dialog;
pub mod model_registry;
pub mod photo_grid;
pub mod setup_window;
pub mod sidebar;
pub mod video_viewer;
pub mod viewer;
pub mod window;

pub use setup_window::MomentsSetupWindow;
pub use window::MomentsWindow;

/// A view that can be placed in the content area of the main shell.
///
/// Each implementor owns its own `AdwToolbarView` + `AdwHeaderBar`, following
/// the Fractal / GNOME Settings pattern of per-view header bars rather than
/// a shared header with swappable controls.
pub trait ContentView {
    /// The root widget to place inside the split-view content pane.
    fn widget(&self) -> &gtk::Widget;
}

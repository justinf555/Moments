pub mod album;
pub mod import_client;
pub mod media;
pub mod people;

pub use album::{AlbumClientV2, AlbumItemObject};
pub use import_client::{ImportClient, ImportState};
pub use media::{MediaClient, MediaItemObject};
pub use people::{PeopleClient, PersonItemObject};

use std::future::Future;

use gtk::glib;
use gtk::prelude::*;

use crate::library::error::LibraryError;
use crate::UserFacingError;

/// Spawn a future on Tokio and flatten the nested `Result`.
///
/// Converts the `Result<Result<T, LibraryError>, JoinError>` from
/// `tokio::spawn` into a single `Result<T, LibraryError>`.
pub async fn spawn_on<T: Send + 'static>(
    tokio: &tokio::runtime::Handle,
    task: impl Future<Output = Result<T, LibraryError>> + Send + 'static,
) -> Result<T, LibraryError> {
    tokio
        .spawn(task)
        .await
        .map_err(|e| LibraryError::Runtime(e.to_string()))?
}

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

/// Show a toast with a user-facing error message.
///
/// Extracts the human-readable message via [`UserFacingError`] so
/// technical details stay in the logs, not the UI.
pub fn show_error_toast(error: &impl UserFacingError) {
    show_toast(&error.to_user_facing());
}

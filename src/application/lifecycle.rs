//! Library-open lifecycle for `MomentsApplication`.
//!
//! Hosts the setup-wizard and open-on-launch entry points that turn a
//! library path into a running window: presenting the first-run setup
//! window, handling its completion, and opening an existing library from
//! a saved path. The shared input → ready-library pipeline lives in
//! `library_loader`; this module orchestrates the window transition and
//! the failure dialogs. See `docs/design-library-context.md`.

use std::path::{Path, PathBuf};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use tracing::{error, instrument};

use super::MomentsApplication;
use crate::application::library_loader::{LibraryLoader, LoadFailure, LoadOutcome};
use crate::application::startup;
use crate::ui::MomentsSetupWindow;
use crate::ui::MomentsWindow;

impl MomentsApplication {
    /// Show the first-run setup window.
    pub(in crate::application) fn show_setup_window(&self) -> MomentsSetupWindow {
        let setup = MomentsSetupWindow::new(self);
        setup.connect_setup_complete(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |win, path| {
                app.on_setup_complete(win, path);
            }
        ));
        setup.present();
        setup
    }

    /// Called when the user completes the setup wizard.
    ///
    /// Creates the bundle, persists the path to GSettings, presents the main
    /// window, closes the setup window, then loads the library asynchronously.
    /// The main window is created before the setup window closes so there is
    /// never a windowless state.
    #[instrument(skip(self, setup_win), fields(path = %path))]
    pub(in crate::application) fn on_setup_complete(
        &self,
        setup_win: &MomentsSetupWindow,
        path: String,
    ) {
        // All setup pages (Local and Immich) create the bundle before
        // emitting setup-complete. The loader opens it and resolves any
        // Immich token from the keyring (the wizard stored it just before
        // emitting setup-complete, so a failure here means the keyring became
        // unavailable between store and lookup).
        let (bundle, config) = match LibraryLoader.load(Path::new(&path)) {
            LoadOutcome::Ready { bundle, config } => (bundle, config),
            LoadOutcome::Failed(failure) => {
                let (heading, body) = setup_failure_dialog(&failure, Path::new(&path));
                show_library_error_dialog(setup_win, &heading, &body);
                return;
            }
        };

        let settings = self.imp().settings.get().expect("settings initialised");
        if let Err(e) = settings.set_string("library-path", &path) {
            error!("failed to save library path to GSettings: {e}");
        }

        // Present the main window first, then close setup — ensures there is
        // always at least one window alive during the transition.
        let window = MomentsWindow::new(self, settings);
        window.present();
        setup_win.close();

        startup::start(self, bundle, config, window);
    }

    /// Open an existing library from a saved path.
    ///
    /// Creates and presents the main window immediately (loading page) so
    /// there is no windowless gap while the async factory runs.
    ///
    /// If the bundle cannot be opened (e.g. the directory was deleted while
    /// the GSettings path entry still exists) the stale path is cleared and
    /// the setup window is shown so the user can reconfigure.
    pub(in crate::application) fn open_library(&self, path: PathBuf) {
        let (bundle, config) = match LibraryLoader.load(&path) {
            LoadOutcome::Ready { bundle, config } => (bundle, config),
            LoadOutcome::Failed(failure) => {
                // Clear the stale path *before* showing the setup window so a
                // missing bundle / missing token does not re-trigger an open
                // on the next launch. Transient keyring failures leave it
                // intact so a relaunch recovers once the keyring is healthy.
                if failure.clear_path() {
                    let settings = self.imp().settings.get().expect("settings initialised");
                    if let Err(e) = settings.set_string("library-path", "") {
                        error!("failed to clear stale library path: {e}");
                    }
                }
                // No window exists yet on this path — show the setup window and
                // parent the explanation dialog to it.
                let setup_win = self.show_setup_window();
                let (heading, body) = open_failure_dialog(&failure, &path);
                show_library_error_dialog(&setup_win, &heading, &body);
                return;
            }
        };

        let settings = self.imp().settings.get().expect("settings initialised");
        let window = MomentsWindow::new(self, settings);
        window.present();

        startup::start(self, bundle, config, window);
    }
}

/// Dialog heading/body for a load failure in the *setup wizard* path.
///
/// Phrased for "the user just finished setup": the bundle was created moments
/// ago, and an Immich token was stored just before this. See
/// `library_loader::LoadFailure`.
fn setup_failure_dialog(failure: &LoadFailure, path: &Path) -> (String, String) {
    match failure {
        LoadFailure::BundleOpen { details } => (
            "Could not open library".to_string(),
            format!(
                "The library at {} could not be opened.\n\nDetails: {details}",
                path.display()
            ),
        ),
        LoadFailure::TokenMissing => (
            gettext("Sign in required"),
            gettext(
                "The session token saved during setup could not be read back. Please try signing in again.",
            ),
        ),
        LoadFailure::KeyringFailed { details } => (
            gettext("Could not access the system keyring"),
            format!(
                "{}\n\nDetails: {details}",
                gettext("Moments stored your session in the keyring but could not read it back. Please check your keyring service and try again.")
            ),
        ),
    }
}

/// Dialog heading/body for a load failure in the *open-on-launch* path.
///
/// Phrased for "a previously-saved library could not be reopened" and
/// includes the offending `path`. See `library_loader::LoadFailure`.
fn open_failure_dialog(failure: &LoadFailure, path: &Path) -> (String, String) {
    match failure {
        LoadFailure::BundleOpen { details } => (
            "Could not open library".to_string(),
            format!(
                "The library at {} could not be opened. Please set up a new library.\n\nDetails: {details}",
                path.display()
            ),
        ),
        LoadFailure::TokenMissing => (
            gettext("Sign in required"),
            gettext(
                "Your saved Immich session was not found in the system keyring. Please sign in again to continue.",
            ),
        ),
        LoadFailure::KeyringFailed { details } => (
            gettext("Could not access the system keyring"),
            format!(
                "{}\n\nDetails: {details}",
                gettext("Moments could not read your saved Immich session. Try restarting the app once your keyring service is available, or sign in again to continue.")
            ),
        ),
    }
}

/// Show a blocking error dialog for library open/create failures.
fn show_library_error_dialog(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.present(Some(parent));
}

// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opening a library bundle: first-run setup completion, reopening the
//! saved library at startup, and the keyring token lookup both need.

use std::path::PathBuf;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use tracing::{debug, error, instrument};

use super::keyring;
use crate::library::bundle::Bundle;
use crate::library::config::LibraryConfig;
use crate::ui::{MomentsSetupWindow, MomentsWindow};

use super::MomentsApplication;

impl MomentsApplication {
    /// Show the first-run setup window.
    pub(super) fn show_setup_window(&self) -> MomentsSetupWindow {
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
    /// Creates the bundle, resolves the session token from the keyring
    /// asynchronously (#704), persists the path to GSettings, presents the
    /// main window, closes the setup window, then loads the library
    /// asynchronously. The setup window stays up (insensitive) while the
    /// keyring answers, and the main window is created before it closes, so
    /// there is never a windowless state.
    #[instrument(skip(self, setup_win), fields(path = %path))]
    fn on_setup_complete(&self, setup_win: &MomentsSetupWindow, path: String) {
        let bundle_path = PathBuf::from(&path);

        // All setup pages (Local and Immich) create the bundle before emitting
        // setup-complete. We just open it here.
        let (bundle, config) = match Bundle::open(&bundle_path) {
            Ok(result) => result,
            Err(e) => {
                error!("failed to open bundle: {e}");
                show_library_error_dialog(
                    setup_win,
                    &gettext("Could not open library"),
                    &with_details(
                        &gettext("The library at {} could not be opened.")
                            .replace("{}", &bundle_path.display().to_string()),
                        &e,
                    ),
                );
                return;
            }
        };

        // Block a second submit while the keyring lookup is in flight.
        setup_win.set_sensitive(false);

        glib::MainContext::default().spawn_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            setup_win,
            async move {
                let token_result = with_immich_token(config).await;
                setup_win.set_sensitive(true);

                // The setup wizard stores the token before emitting
                // setup-complete, so a failure here means the keyring became
                // unavailable between store and lookup — surface it rather
                // than booting the user into a library that can never
                // authenticate.
                let config = match token_result {
                    Ok(config) => config,
                    Err(err) => {
                        let (heading, body) = match err {
                            keyring::TokenError::Missing => (
                                gettext("Sign in required"),
                                gettext(
                                    "The session token saved during setup could not be read back. Please try signing in again.",
                                ),
                            ),
                            keyring::TokenError::KeyringFailed(e) => {
                                error!("keyring lookup failed immediately after setup: {e}");
                                (
                                    gettext("Could not access the system keyring"),
                                    with_details(
                                        &gettext("Moments stored your session in the keyring but could not read it back. Please check your keyring service and try again."),
                                        &e,
                                    ),
                                )
                            }
                        };
                        show_library_error_dialog(&setup_win, &heading, &body);
                        return;
                    }
                };

                let settings = app.settings();
                if let Err(e) = settings.set_string("library-path", &path) {
                    error!("failed to save library path to GSettings: {e}");
                }

                // Present the main window first, then close setup — ensures
                // there is always at least one window alive during the
                // transition.
                let window = MomentsWindow::new(&app, &settings);
                window.present();
                setup_win.close();

                app.load_library_async(bundle, config, window);
            }
        ));
    }

    /// Open an existing library from a saved path.
    ///
    /// Creates and presents the main window immediately (loading page) so
    /// there is no windowless gap while the keyring lookup and the async
    /// factory run.
    ///
    /// If the bundle cannot be opened (e.g. the directory was deleted while
    /// the GSettings path entry still exists) the stale path is cleared and
    /// the setup window is shown so the user can reconfigure.
    pub(super) fn open_library(&self, path: PathBuf) {
        let (bundle, config) = match Bundle::open(&path) {
            Ok(result) => result,
            Err(e) => {
                error!("failed to open library bundle: {e}");
                self.clear_library_path();
                // Show setup window with an error dialog explaining what happened.
                let setup_win = self.show_setup_window();
                show_library_error_dialog(
                    &setup_win,
                    &gettext("Could not open library"),
                    &with_details(
                        &gettext(
                            "The library at {} could not be opened. Please set up a new library.",
                        )
                        .replace("{}", &path.display().to_string()),
                        &e,
                    ),
                );
                return;
            }
        };

        // Present the main window (loading page) before touching the keyring:
        // the lookup is a D-Bus round trip that can take seconds if the secret
        // service is still starting or shows an unlock prompt (#704).
        let window = MomentsWindow::new(self, &self.settings());
        window.present();
        let window_weak = window.downgrade();

        glib::MainContext::default().spawn_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                let token_result = with_immich_token(config).await;

                // The user closed the window while the keyring was answering;
                // with no window left the app is already shutting down.
                let Some(window) = window_weak.upgrade() else {
                    debug!("main window closed before keyring lookup finished");
                    return;
                };

                // If the token cannot be resolved, refuse to load the
                // library — passing an empty string downstream produces
                // opaque 401 toasts and keeps the outbox spinning. Bounce the
                // user back to the setup window with an explanation instead.
                let config = match token_result {
                    Ok(config) => config,
                    Err(err) => {
                        // Only `Missing` warrants clearing `library-path`: the
                        // user has no credential for this server and must
                        // re-run setup. `KeyringFailed` is typically transient
                        // (D-Bus race during session start, locked collection
                        // prompt timeout) — leave the saved path intact so a
                        // simple relaunch recovers once the keyring is healthy.
                        //
                        // Show setup before closing the main window so the app
                        // is never windowless.
                        let setup_win = app.show_setup_window();
                        window.close();
                        let (heading, body) = match err {
                            keyring::TokenError::Missing => {
                                app.clear_library_path();
                                (
                                    gettext("Sign in required"),
                                    gettext(
                                        "Your saved Immich session was not found in the system keyring. Please sign in again to continue.",
                                    ),
                                )
                            }
                            keyring::TokenError::KeyringFailed(e) => {
                                error!("keyring lookup failed during open_library: {e}");
                                (
                                    gettext("Could not access the system keyring"),
                                    with_details(
                                        &gettext("Moments could not read your saved Immich session. Try restarting the app once your keyring service is available, or sign in again to continue."),
                                        &e,
                                    ),
                                )
                            }
                        };
                        show_library_error_dialog(&setup_win, &heading, &body);
                        return;
                    }
                };

                app.load_library_async(bundle, config, window);
            }
        ));
    }

    /// Forget the saved library so the next launch shows the setup window.
    fn clear_library_path(&self) {
        if let Err(e) = self.settings().set_string("library-path", "") {
            error!("failed to clear stale library path: {e}");
        }
    }
}

/// Show a blocking error dialog for library open/create failures.
fn show_library_error_dialog(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    dialog.add_response("ok", &gettext("OK"));
    dialog.set_default_response(Some("ok"));
    dialog.present(Some(parent));
}

/// Append the technical error to a user-facing message, for dialog bodies.
pub(super) fn with_details(message: &str, err: &impl std::fmt::Display) -> String {
    format!(
        "{message}\n\n{}",
        gettext("Details: {}").replace("{}", &err.to_string())
    )
}

/// For an Immich config, inject the session token from the keyring.
///
/// The bundle's `library.toml` doesn't hold the token, so the config read
/// from it has an empty `access_token`. Local configs pass through.
/// Must be awaited on the GTK main context (the keyring future is `!Send`).
async fn with_immich_token(config: LibraryConfig) -> Result<LibraryConfig, keyring::TokenError> {
    match config {
        LibraryConfig::Immich { server_url, .. } => {
            let access_token = keyring::resolve_immich_token(&server_url).await?;
            Ok(LibraryConfig::Immich {
                server_url,
                access_token,
            })
        }
        other => Ok(other),
    }
}

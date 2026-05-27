/* application.rs
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

pub mod keyring;

mod context;
mod startup;

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::{debug, error, info, instrument, warn};

use crate::application::context::LibraryContext;
use crate::config::{APP_ID, PROFILE, VERSION};
use crate::library::bundle::Bundle;
use crate::library::config::LibraryConfig;
use crate::sync::SyncEngine;
use crate::ui::MomentsSetupWindow;
use crate::ui::MomentsWindow;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MomentsApplication {
        pub settings: OnceCell<gio::Settings>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
        /// Domain + infrastructure container.
        ///
        /// Stored in a `RefCell<Option<...>>` rather than the
        /// `OnceCell<Arc<...>>` sketched in the design doc so the
        /// shutdown path can clear it and drop `Arc<Library>` (and the
        /// `SqlitePool` it wraps) before `main()` drops the Tokio
        /// runtime. The `OnceCell` shape can be revisited after Step 6
        /// reshapes `sync_handle`. See `docs/design-library-context.md`.
        pub(in crate::application) library_context: RefCell<Option<Arc<LibraryContext>>>,
        // Client GObject singletons. Set once during
        // `startup::phase4_install` via `OnceCell::set` and read for
        // the rest of the application lifetime. `sync_client` is left
        // optional (no `.set()` call on the Local backend); the other
        // clients are always populated before the main window is
        // wired up, so accessors panic if read pre-init. See
        // `docs/design-library-context.md` Step 3.
        pub import_client: OnceCell<crate::client::ImportClient>,
        pub album_client_v2: OnceCell<crate::client::AlbumClientV2>,
        pub people_client: OnceCell<crate::client::PeopleClientV2>,
        pub media_client_v2: OnceCell<crate::client::MediaClientV2>,
        pub sync_client: OnceCell<crate::client::SyncClient>,
        pub is_immich: Cell<bool>,
        pub immich_server_url: RefCell<Option<String>>,
        // Note: the periodic trash-purge `JoinHandle` lives on
        // `LibraryContext::purge_handle`, not here. `JoinHandle` is not
        // `Clone`, so the context is the single canonical owner per
        // `docs/design-library-context.md`.
        /// Long-running Immich sync service. Populated by
        /// `startup::phase4_install` only for Immich-backed libraries;
        /// never set on the Local backend. `SyncClient` holds its own
        /// `Arc<SyncEngine>` so widgets that need engine control go
        /// through the client rather than this field. See
        /// `docs/design-library-context.md` Step 6.
        pub(in crate::application) sync_engine: OnceCell<Arc<SyncEngine>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsApplication {
        const NAME: &'static str = "MomentsApplication";
        type Type = super::MomentsApplication;
        type ParentType = adw::Application;
    }

    // TODO: convert all client fields to proper GObject properties
    // during the Application refactor. For now we only register
    // sync-client and import-client so ActivityIndicator can listen
    // for notify signals.
    impl ObjectImpl for MomentsApplication {
        fn properties() -> &'static [glib::ParamSpec] {
            use std::sync::OnceLock;
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecObject::builder::<crate::client::SyncClient>("sync-client")
                        .read_only()
                        .build(),
                    glib::ParamSpecObject::builder::<crate::client::ImportClient>("import-client")
                        .read_only()
                        .build(),
                ]
            })
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "sync-client" => self.sync_client.get().to_value(),
                "import-client" => self.import_client.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.setup_gactions();
            obj.set_accels_for_action("app.quit", &["<control>q"]);
            obj.set_accels_for_action("app.import", &["<control>i"]);
            obj.set_accels_for_action("app.preferences", &["<control>comma"]);
            obj.set_accels_for_action("app.shortcuts", &["<control>question"]);
            // F9 is handled by the viewer's EventControllerKey for the info
            // panel toggle — don't register it as a global accelerator here.
            obj.set_accels_for_action(
                "view.zoom-in",
                &["<control>equal", "<control>plus", "<control>KP_Add"],
            );
            obj.set_accels_for_action("view.zoom-out", &["<control>minus", "<control>KP_Subtract"]);
        }
    }

    impl ApplicationImpl for MomentsApplication {
        fn shutdown(&self) {
            info!("application shutting down");

            // Drop all library-related state so the Arc<Library>
            // (and the SqlitePool it wraps) is freed before drop(tokio)
            // in main() tries to shut down the runtime.
            // Shut down sync engine explicitly before dropping clients.
            //
            // `sync_engine` is a `OnceCell<Arc<SyncEngine>>`, so we
            // cannot remove the Arc — but the engine's spawned tasks
            // observe the shutdown flag at their next polling
            // boundary and exit. The remaining `Arc<SyncEngine>`
            // clones (this one and the one held by `SyncClient`) are
            // dropped when the `MomentsApplication` itself is dropped,
            // after `shutdown` returns.
            if let Some(engine) = self.sync_engine.get() {
                engine.shutdown();
            }

            // Drop the LibraryContext — it owns the canonical
            // `Arc<Library>` (and the `Arc<RenderPipeline>`) and the
            // purge-task `JoinHandle`. Aborting the purge task before
            // dropping the runtime avoids the (unlikely but possible)
            // race where it wakes and touches the DB after the pool
            // has been freed.
            //
            // Client GObject singletons are stored in `OnceCell<T>` on
            // `imp` and cannot be cleared from `&self`; they still
            // hold their own `Arc<Library>` clones until the
            // `MomentsApplication` itself drops. That is acceptable:
            // the `SqlitePool` is reference-counted and only the
            // *runtime* needs to outlive the *last* `Arc<Library>`
            // drop, which happens during `MomentsApplication` drop —
            // after `shutdown` returns but before `main()` drops the
            // runtime.
            if let Some(ctx) = self.library_context.borrow_mut().take() {
                if let Some(handle) = ctx.purge_handle() {
                    handle.abort();
                }
            }

            self.parent_shutdown();
        }

        fn activate(&self) {
            let app = self.obj();

            // Load custom CSS (selection highlighting, etc.).
            let provider = gtk::CssProvider::new();
            provider.load_from_resource("/io/github/justinf555/Moments/style.css");
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }

            // Apply saved color scheme preference.
            let settings = self.settings.get_or_init(|| gio::Settings::new(APP_ID));
            let color_scheme = match settings.uint("color-scheme") {
                1 => adw::ColorScheme::ForceLight,
                4 => adw::ColorScheme::ForceDark,
                _ => adw::ColorScheme::Default,
            };
            adw::StyleManager::default().set_color_scheme(color_scheme);

            // Present existing window if the app is already running.
            if let Some(window) = app.active_window() {
                window.present();
                return;
            }

            let settings = self.settings.get_or_init(|| gio::Settings::new(APP_ID));

            let library_path = settings.string("library-path");

            if library_path.is_empty() {
                info!("no library configured, showing setup window");
                app.show_setup_window();
            } else {
                info!(path = %library_path, "opening existing library");
                app.open_library(PathBuf::from(library_path.as_str()));
            }
        }
    }

    impl GtkApplicationImpl for MomentsApplication {}
    impl AdwApplicationImpl for MomentsApplication {}
}

glib::wrapper! {
    pub struct MomentsApplication(ObjectSubclass<imp::MomentsApplication>)
        @extends gio::Application, gtk::Application, adw::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl MomentsApplication {
    pub fn new(
        application_id: &str,
        flags: &gio::ApplicationFlags,
        tokio: tokio::runtime::Handle,
    ) -> Self {
        let app: Self = glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", flags)
            .property("resource-base-path", "/io/github/justinf555/Moments")
            .build();
        app.imp()
            .tokio
            .set(tokio)
            .expect("tokio handle set once at construction");
        app
    }

    /// Access the shared Tokio runtime handle.
    ///
    /// Available from anywhere via `MomentsApplication::default().tokio_handle()`.
    ///
    /// Note: the `tokio` handle is set on the application at construction
    /// — long before `LibraryContext` exists — so this accessor reads
    /// the original `OnceCell<tokio::runtime::Handle>` field. The context
    /// only sees the same handle once it has been built; it is not the
    /// canonical source for this value in Step 1.
    pub fn tokio_handle(&self) -> tokio::runtime::Handle {
        self.imp().tokio.get().expect("tokio handle set").clone()
    }

    /// Access the domain + infrastructure container.
    ///
    /// Visible only inside the `application/` module tree. UI code must
    /// go through a Client; other backend wiring code may call this.
    /// Returns `None` if no library has been opened yet.
    ///
    /// Today `startup::phase4_install` works with the local
    /// `Arc<LibraryContext>` it received from phase 1, so the only
    /// in-tree caller of this accessor is `shutdown`. See
    /// `docs/design-library-context.md`.
    #[allow(dead_code)]
    pub(in crate::application) fn library_context(&self) -> Option<Arc<LibraryContext>> {
        self.imp().library_context.borrow().clone()
    }

    /// Access the import client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().import_client()`.
    /// Panics if called before `startup::start` has populated the client.
    pub fn import_client(&self) -> &crate::client::ImportClient {
        self.imp()
            .import_client
            .get()
            .expect("import_client accessed before library was opened")
    }

    /// Access the album client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().album_client_v2()`.
    /// Panics if called before `startup::start` has populated the client.
    pub fn album_client_v2(&self) -> &crate::client::AlbumClientV2 {
        self.imp()
            .album_client_v2
            .get()
            .expect("album_client_v2 accessed before library was opened")
    }

    /// Access the people client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().people_client()`.
    /// Panics if called before `startup::start` has populated the client.
    pub fn people_client(&self) -> &crate::client::PeopleClientV2 {
        self.imp()
            .people_client
            .get()
            .expect("people_client accessed before library was opened")
    }

    /// Access the media client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().media_client_v2()`.
    /// Panics if called before `startup::start` has populated the client.
    pub fn media_client_v2(&self) -> &crate::client::MediaClientV2 {
        self.imp()
            .media_client_v2
            .get()
            .expect("media_client_v2 accessed before library was opened")
    }

    /// Access the sync client singleton (Immich only).
    ///
    /// Returns `None` for local libraries or if no library is open yet.
    pub fn sync_client(&self) -> Option<&crate::client::SyncClient> {
        self.imp().sync_client.get()
    }

    /// Store the sync client and notify listeners.
    pub fn set_sync_client(&self, client: crate::client::SyncClient) {
        self.imp()
            .sync_client
            .set(client)
            .expect("sync_client set at most once per application lifetime");
        self.notify("sync-client");
    }

    /// Store the import client and notify listeners.
    pub fn set_import_client(&self, client: crate::client::ImportClient) {
        self.imp()
            .import_client
            .set(client)
            .expect("import_client set at most once per application lifetime");
        self.notify("import-client");
    }

    /// Update the sync polling interval. No-op if no sync engine is
    /// running. Routes through the `SyncClient`'s `Arc<SyncEngine>`
    /// reference so the preferences dialog never sees the engine
    /// directly.
    pub fn set_sync_interval(&self, secs: u64) {
        if let Some(client) = self.imp().sync_client.get() {
            client.set_interval(secs);
        }
    }

    /// Get the singleton application instance.
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Self {
        gio::Application::default()
            .and_downcast::<Self>()
            .expect("application is MomentsApplication")
    }

    fn setup_gactions(&self) {
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

    /// Show the first-run setup window.
    fn show_setup_window(&self) -> MomentsSetupWindow {
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
                    "Could not open library",
                    &format!(
                        "The library at {} could not be opened.\n\nDetails: {e}",
                        bundle_path.display()
                    ),
                );
                return;
            }
        };

        // For Immich configs, inject the session token from the keyring.
        // The setup wizard stores the token before emitting setup-complete,
        // so a failure here means the keyring became unavailable between
        // store and lookup — surface it rather than booting the user into a
        // library that can never authenticate.
        let config = match config {
            LibraryConfig::Immich { server_url, .. } => {
                match keyring::resolve_immich_token(&server_url) {
                    Ok(access_token) => LibraryConfig::Immich {
                        server_url,
                        access_token,
                    },
                    Err(err) => {
                        let (heading, body) = match err {
                            keyring::TokenError::Missing => (
                                gettext("Sign in required"),
                                gettext(
                                    "The session token saved during setup could not be read back. Please try signing in again.",
                                ),
                            ),
                            keyring::TokenError::KeyringFailed(e) => {
                                error!(
                                    "keyring lookup failed immediately after setup: {e}"
                                );
                                (
                                    gettext("Could not access the system keyring"),
                                    format!(
                                        "{}\n\nDetails: {e}",
                                        gettext("Moments stored your session in the keyring but could not read it back. Please check your keyring service and try again.")
                                    ),
                                )
                            }
                        };
                        show_library_error_dialog(setup_win, &heading, &body);
                        return;
                    }
                }
            }
            other => other,
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
    fn open_library(&self, path: PathBuf) {
        let (bundle, config) = match Bundle::open(&path) {
            Ok(result) => result,
            Err(e) => {
                error!("failed to open library bundle: {e}");
                let settings = self.imp().settings.get().expect("settings initialised");
                if let Err(e) = settings.set_string("library-path", "") {
                    error!("failed to clear stale library path: {e}");
                }
                // Show setup window with an error dialog explaining what happened.
                let setup_win = self.show_setup_window();
                show_library_error_dialog(
                    &setup_win,
                    "Could not open library",
                    &format!(
                        "The library at {} could not be opened. Please set up a new library.\n\nDetails: {e}",
                        path.display()
                    ),
                );
                return;
            }
        };

        // For Immich configs, inject the session token from the keyring.
        // If the token cannot be resolved, refuse to load the library — passing
        // an empty string downstream produces opaque 401 toasts and keeps the
        // outbox spinning. Bounce the user back to the setup window with an
        // explanation instead.
        let config = match config {
            LibraryConfig::Immich { server_url, .. } => {
                match keyring::resolve_immich_token(&server_url) {
                    Ok(access_token) => LibraryConfig::Immich {
                        server_url,
                        access_token,
                    },
                    Err(err) => {
                        // Only `Missing` warrants clearing `library-path`: the
                        // user has no credential for this server and must
                        // re-run setup. `KeyringFailed` is typically transient
                        // (D-Bus race during session start, locked collection
                        // prompt timeout) — leave the saved path intact so a
                        // simple relaunch recovers once the keyring is healthy.
                        let setup_win = self.show_setup_window();
                        let (heading, body) = match err {
                            keyring::TokenError::Missing => {
                                let settings =
                                    self.imp().settings.get().expect("settings initialised");
                                if let Err(e) = settings.set_string("library-path", "") {
                                    error!("failed to clear stale library path: {e}");
                                }
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
                                    format!(
                                        "{}\n\nDetails: {e}",
                                        gettext("Moments could not read your saved Immich session. Try restarting the app once your keyring service is available, or sign in again to continue.")
                                    ),
                                )
                            }
                        };
                        show_library_error_dialog(&setup_win, &heading, &body);
                        return;
                    }
                }
            }
            other => other,
        };

        let settings = self.imp().settings.get().expect("settings initialised");
        let window = MomentsWindow::new(self, settings);
        window.present();

        startup::start(self, bundle, config, window);
    }

    /// Open a folder picker and start importing the selected folder.
    fn show_import_dialog(&self) {
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

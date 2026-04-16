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

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::{debug, error, info, instrument, warn};

use crate::app_event::AppEvent;
use crate::config::{APP_ID, PROFILE, VERSION};
use crate::event_bus::EventBus;
use crate::library::bundle::Bundle;
use crate::library::config::LibraryConfig;
use crate::library::Library;
use crate::ui::MomentsSetupWindow;
use crate::ui::MomentsWindow;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MomentsApplication {
        pub settings: OnceCell<gio::Settings>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
        pub library: RefCell<Option<Arc<Library>>>,
        pub import_client: RefCell<Option<crate::client::import_client::ImportClient>>,
        pub album_client_v2: RefCell<Option<crate::client::AlbumClientV2>>,
        pub people_client: RefCell<Option<crate::client::PeopleClient>>,
        pub media_client: RefCell<Option<crate::client::MediaClient>>,
        pub render_pipeline: RefCell<Option<Arc<crate::renderer::pipeline::RenderPipeline>>>,
        pub is_immich: Cell<bool>,
        pub immich_server_url: RefCell<Option<String>>,
        /// Centralised event bus for fan-out event delivery.
        /// Created when the library is loaded.
        pub event_bus: RefCell<Option<EventBus>>,
        /// App-lifetime event bus subscriptions (error toasts, etc.).
        pub subscriptions: RefCell<Vec<crate::event_bus::Subscription>>,
        /// Background task handle for periodic trash purge.
        pub purge_handle: RefCell<Option<tokio::task::JoinHandle<()>>>,
        /// Sync engine handle (Immich only).
        pub sync_handle: RefCell<Option<crate::sync::SyncHandle>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsApplication {
        const NAME: &'static str = "MomentsApplication";
        type Type = super::MomentsApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for MomentsApplication {
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
            self.event_bus.borrow_mut().take();
            self.album_client_v2.borrow_mut().take();
            self.people_client.borrow_mut().take();
            self.media_client.borrow_mut().take();
            self.library.borrow_mut().take();

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
    pub fn tokio_handle(&self) -> tokio::runtime::Handle {
        self.imp().tokio.get().expect("tokio handle set").clone()
    }

    /// Access the import client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().import_client()`.
    /// Returns `None` if no library is open yet.
    pub fn import_client(&self) -> Option<crate::client::import_client::ImportClient> {
        self.imp().import_client.borrow().clone()
    }

    /// Access the shared render pipeline.
    ///
    /// Available from anywhere via `MomentsApplication::default().render_pipeline()`.
    /// Returns `None` if no library is open yet.
    pub fn render_pipeline(&self) -> Option<Arc<crate::renderer::pipeline::RenderPipeline>> {
        self.imp().render_pipeline.borrow().clone()
    }

    /// Access the album client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().album_client_v2()`.
    /// Returns `None` if no library is open yet.
    pub fn album_client_v2(&self) -> Option<crate::client::AlbumClientV2> {
        self.imp().album_client_v2.borrow().clone()
    }

    /// Access the people client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().people_client()`.
    /// Returns `None` if no library is open yet.
    pub fn people_client(&self) -> Option<crate::client::PeopleClient> {
        self.imp().people_client.borrow().clone()
    }

    /// Access the media client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().media_client()`.
    /// Returns `None` if no library is open yet.
    pub fn media_client(&self) -> Option<crate::client::MediaClient> {
        self.imp().media_client.borrow().clone()
    }

    /// Update the sync polling interval. No-op if no sync engine is running.
    pub fn set_sync_interval(&self, secs: u64) {
        if let Some(ref handle) = *self.imp().sync_handle.borrow() {
            handle.set_interval(secs);
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
        let config = match config {
            LibraryConfig::Immich { server_url, .. } => {
                let access_token = keyring::lookup_access_token(&server_url)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                LibraryConfig::Immich {
                    server_url,
                    access_token,
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

        self.load_library_async(bundle, config, window);
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
        let config = match config {
            LibraryConfig::Immich { server_url, .. } => {
                let access_token = keyring::lookup_access_token(&server_url)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                LibraryConfig::Immich {
                    server_url,
                    access_token,
                }
            }
            other => other,
        };

        let settings = self.imp().settings.get().expect("settings initialised");
        let window = MomentsWindow::new(self, settings);
        window.present();

        self.load_library_async(bundle, config, window);
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
        let import_client = match self.imp().import_client.borrow().clone() {
            Some(c) => c,
            None => {
                error!("import requested but no library is open");
                return;
            }
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

    /// Spawn the async factory call on the glib main context.
    ///
    /// On success:
    ///  1. Creates the event bus and opens the library backend.
    ///  2. Wires the shell (sidebar, views, command dispatcher).
    ///  3. Switches the window to its content page.
    fn load_library_async(&self, bundle: Bundle, config: LibraryConfig, window: MomentsWindow) {
        // Extract Immich connection info before the config is consumed.
        let immich_info = match &config {
            LibraryConfig::Immich {
                server_url,
                access_token,
            } => Some((server_url.clone(), access_token.clone())),
            _ => None,
        };

        // Store backend type for preferences dialog.
        if let Some((ref server_url, _)) = immich_info {
            self.imp().is_immich.set(true);
            *self.imp().immich_server_url.borrow_mut() = Some(server_url.clone());
        }

        // Extract paths and mode for the ImportClient before the factory
        // consumes the bundle and config.
        let originals_dir = bundle.originals.clone();
        let thumbnails_dir = bundle.thumbnails.clone();
        let storage_mode = match &config {
            LibraryConfig::Local { mode } => mode.clone(),
            LibraryConfig::Immich { .. } => crate::library::config::LocalStorageMode::Managed,
        };

        glib::MainContext::default().spawn_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            window,
            async move {
                let tokio = app.imp().tokio.get().expect("tokio handle set").clone();

                let bus = EventBus::new();

                let import_mode = storage_mode.clone();
                let db = crate::library::db::Database::new();

                // Build Immich client + recorder + resolver based on config.
                let immich_client = immich_info.as_ref().and_then(|(url, token)| {
                    crate::sync::providers::immich::client::ImmichClient::new(url, token).ok()
                });

                let recorder: std::sync::Arc<dyn crate::library::recorder::MutationRecorder> =
                    if immich_client.is_some() {
                        std::sync::Arc::new(crate::sync::outbox::QueueWriterOutbox::new(db.clone()))
                    } else {
                        std::sync::Arc::new(crate::sync::outbox::NoOpRecorder)
                    };

                let resolver: std::sync::Arc<dyn crate::library::resolver::OriginalResolver> =
                    if let Some(ref client) = immich_client {
                        std::sync::Arc::new(
                            crate::sync::providers::immich::resolver::CachedResolver::new(
                                std::sync::Arc::new(client.clone()),
                                originals_dir.clone(),
                            ),
                        )
                    } else {
                        std::sync::Arc::new(crate::library::resolver::LocalResolver::new(
                            originals_dir.clone(),
                            import_mode.clone(),
                        ))
                    };

                let db_for_sync = db.clone();
                let open_result = tokio
                    .spawn(async move {
                        Library::open(bundle, storage_mode, db, recorder, resolver).await
                    })
                    .await
                    .map_err(|e| crate::library::error::LibraryError::Runtime(e.to_string()));
                let storage_mode = import_mode;
                match open_result.and_then(|r| r) {
                    Ok(library) => {
                        let library = Arc::new(library);
                        info!("library ready");

                        // Store library on the application.
                        *app.imp().library.borrow_mut() = Some(Arc::clone(&library));

                        // Notify the UI that the library is ready.
                        bus.sender().send(AppEvent::Ready);

                        // Create the import client (GObject singleton).
                        let sync_thumbnails_dir = thumbnails_dir.clone();
                        {
                            let render_pipeline = std::sync::Arc::new(
                                crate::renderer::pipeline::RenderPipeline::new(),
                            );
                            *app.imp().render_pipeline.borrow_mut() =
                                Some(Arc::clone(&render_pipeline));

                            let import_client = crate::client::import_client::ImportClient::new();
                            import_client.configure(
                                Arc::clone(&library),
                                originals_dir,
                                thumbnails_dir,
                                Arc::clone(&render_pipeline),
                                storage_mode,
                                tokio.clone(),
                                bus.sender(),
                            );
                            *app.imp().import_client.borrow_mut() = Some(import_client);
                        }

                        // Create the album client (GObject singleton).
                        // Subscribe to AlbumEvent for reactive model updates.
                        {
                            let albums_rx = library.albums().subscribe().await;
                            let album_client_v2 = crate::client::AlbumClientV2::new();
                            album_client_v2.configure(
                                Arc::clone(&library),
                                tokio.clone(),
                                albums_rx,
                            );
                            *app.imp().album_client_v2.borrow_mut() = Some(album_client_v2);
                        }

                        // Create the people client (GObject singleton).
                        {
                            let people_client = crate::client::PeopleClient::new();
                            people_client.configure(Arc::clone(&library), tokio.clone());
                            *app.imp().people_client.borrow_mut() = Some(people_client);
                        }

                        // Create the media client (GObject singleton).
                        {
                            let media_client = crate::client::MediaClient::new();
                            media_client.configure(
                                Arc::clone(&library),
                                tokio.clone(),
                                bus.sender(),
                            );
                            *app.imp().media_client.borrow_mut() = Some(media_client);
                        }

                        // Wire the shell: builds sidebar, registers views,
                        // and switches to the content page. All components
                        // subscribe to the bus for event delivery.
                        let settings = app
                            .imp()
                            .settings
                            .get()
                            .expect("settings initialised")
                            .clone();
                        window.setup(settings, &bus);

                        // Subscribe for command events — routes *Requested
                        // events to library calls on the Tokio runtime.
                        let Some(lib) = app.imp().library.borrow().as_ref().map(Arc::clone) else {
                            tracing::error!("library not initialised when subscribing commands");
                            return;
                        };
                        let cmd_sub =
                            crate::library::commands::subscribe_commands(lib, tokio.clone(), &bus);
                        app.imp().subscriptions.borrow_mut().push(cmd_sub);

                        // Subscribe for error toasts — centralised error
                        // handling for all command failures.
                        {
                            let win_weak = window.downgrade();
                            let sub = bus.subscribe(move |event| {
                                if let AppEvent::Error(msg) = event {
                                    if let Some(win) = win_weak.upgrade() {
                                        gtk::prelude::WidgetExt::activate_action(
                                            &win,
                                            "win.show-toast",
                                            Some(&msg.to_variant()),
                                        )
                                        .ok();
                                    }
                                }
                            });
                            app.imp().subscriptions.borrow_mut().push(sub);
                        }

                        // Start periodic trash purge task.
                        {
                            let lib = Arc::clone(
                                app.imp().library.borrow().as_ref().expect("library set"),
                            );
                            let retention_days = app
                                .imp()
                                .settings
                                .get()
                                .expect("settings initialised")
                                .uint("trash-retention-days");
                            let handle = crate::tasks::purge_trash::start(
                                lib,
                                bus.sender(),
                                retention_days,
                                tokio.clone(),
                            );
                            *app.imp().purge_handle.borrow_mut() = Some(handle);
                        }

                        // Start Immich sync engine if applicable.
                        if let Some(client) = immich_client {
                            let lib = Arc::clone(
                                app.imp().library.borrow().as_ref().expect("library set"),
                            );
                            let sync_interval = app
                                .imp()
                                .settings
                                .get()
                                .expect("settings initialised")
                                .uint("sync-interval-seconds")
                                as u64;
                            let handle = crate::sync::SyncHandle::start(
                                client,
                                lib,
                                db_for_sync,
                                bus.sender(),
                                sync_thumbnails_dir,
                                sync_interval,
                                tokio.clone(),
                            );
                            *app.imp().sync_handle.borrow_mut() = Some(handle);
                        }

                        // Store bus for shutdown cleanup.
                        *app.imp().event_bus.borrow_mut() = Some(bus);
                    }
                    Err(e) => {
                        error!("failed to open library: {e}");

                        let dialog = adw::AlertDialog::builder()
                            .heading("Could not open library")
                            .body(format!(
                                "An error occurred while opening the library.\n\nDetails: {e}"
                            ))
                            .build();
                        dialog.add_response("setup", "Set Up Library");
                        dialog.add_response("quit", "Quit");
                        dialog
                            .set_response_appearance("quit", adw::ResponseAppearance::Destructive);
                        dialog.set_default_response(Some("setup"));
                        dialog.set_close_response("setup");

                        let app_weak = app.downgrade();
                        let win_weak = window.downgrade();
                        dialog.connect_response(None, move |_, response| {
                            if response == "setup" {
                                if let Some(app) = app_weak.upgrade() {
                                    if let Some(win) = win_weak.upgrade() {
                                        win.close();
                                    }
                                    app.show_setup_window();
                                }
                            } else if let Some(app) = app_weak.upgrade() {
                                app.quit();
                            }
                        });

                        dialog.present(Some(&window));
                    }
                }
            }
        ));
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

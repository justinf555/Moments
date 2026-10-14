// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod keyring;

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::{debug, error, info, instrument, warn};

use crate::config::{APP_ID, APP_NAME, VERSION};
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
        pub import_client: RefCell<Option<crate::client::ImportClient>>,
        pub album_client_v2: RefCell<Option<crate::client::AlbumClientV2>>,
        pub people_client: RefCell<Option<crate::client::PeopleClientV2>>,
        pub media_client_v2: RefCell<Option<crate::client::MediaClientV2>>,
        pub sync_client: RefCell<Option<crate::client::SyncClient>>,
        pub render_pipeline: RefCell<Option<Arc<crate::renderer::pipeline::RenderPipeline>>>,
        pub is_immich: Cell<bool>,
        pub immich_server_url: RefCell<Option<String>>,
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
                "sync-client" => self.sync_client.borrow().to_value(),
                "import-client" => self.import_client.borrow().to_value(),
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

            // Release every holder of the Arc<Library> (and the SqlitePool
            // it wraps) before drop(tokio) in main() shuts the runtime down.
            // The purge task and sync engine hold their own references, so
            // stop them before dropping the clients.
            if let Some(handle) = self.purge_handle.borrow_mut().take() {
                handle.abort();
            }
            if let Some(ref handle) = *self.sync_handle.borrow() {
                handle.shutdown();
            }

            self.sync_client.borrow_mut().take();
            self.import_client.borrow_mut().take();
            self.album_client_v2.borrow_mut().take();
            self.people_client.borrow_mut().take();
            self.media_client_v2.borrow_mut().take();
            self.sync_handle.borrow_mut().take();
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

    /// The app's GSettings, created on first `activate`.
    fn settings(&self) -> gio::Settings {
        self.imp()
            .settings
            .get()
            .expect("settings initialised")
            .clone()
    }

    /// Access the import client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().import_client()`.
    /// Returns `None` if no library is open yet.
    pub fn import_client(&self) -> Option<crate::client::ImportClient> {
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
    pub fn people_client(&self) -> Option<crate::client::PeopleClientV2> {
        self.imp().people_client.borrow().clone()
    }

    /// Access the media client singleton.
    ///
    /// Available from anywhere via `MomentsApplication::default().media_client_v2()`.
    /// Returns `None` if no library is open yet.
    pub fn media_client_v2(&self) -> Option<crate::client::MediaClientV2> {
        self.imp().media_client_v2.borrow().clone()
    }

    /// Access the sync client singleton (Immich only).
    ///
    /// Returns `None` for local libraries or if no library is open yet.
    pub fn sync_client(&self) -> Option<crate::client::SyncClient> {
        self.imp().sync_client.borrow().clone()
    }

    /// Store the sync client and notify listeners.
    pub fn set_sync_client(&self, client: crate::client::SyncClient) {
        *self.imp().sync_client.borrow_mut() = Some(client);
        self.notify("sync-client");
    }

    /// Store the import client and notify listeners.
    pub fn set_import_client(&self, client: crate::client::ImportClient) {
        *self.imp().import_client.borrow_mut() = Some(client);
        self.notify("import-client");
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

        // The setup wizard stores the token before emitting setup-complete,
        // so a failure here means the keyring became unavailable between
        // store and lookup — surface it rather than booting the user into a
        // library that can never authenticate.
        let config = match with_immich_token(config) {
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
                show_library_error_dialog(setup_win, &heading, &body);
                return;
            }
        };

        let settings = self.settings();
        if let Err(e) = settings.set_string("library-path", &path) {
            error!("failed to save library path to GSettings: {e}");
        }

        // Present the main window first, then close setup — ensures there is
        // always at least one window alive during the transition.
        let window = MomentsWindow::new(self, &settings);
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

        // If the token cannot be resolved, refuse to load the library — passing
        // an empty string downstream produces opaque 401 toasts and keeps the
        // outbox spinning. Bounce the user back to the setup window with an
        // explanation instead.
        let config = match with_immich_token(config) {
            Ok(config) => config,
            Err(err) => {
                // Only `Missing` warrants clearing `library-path`: the user has
                // no credential for this server and must re-run setup.
                // `KeyringFailed` is typically transient (D-Bus race during
                // session start, locked collection prompt timeout) — leave the
                // saved path intact so a simple relaunch recovers once the
                // keyring is healthy.
                let setup_win = self.show_setup_window();
                let (heading, body) = match err {
                    keyring::TokenError::Missing => {
                        self.clear_library_path();
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

        let window = MomentsWindow::new(self, &self.settings());
        window.present();

        self.load_library_async(bundle, config, window);
    }

    /// Forget the saved library so the next launch shows the setup window.
    fn clear_library_path(&self) {
        if let Err(e) = self.settings().set_string("library-path", "") {
            error!("failed to clear stale library path: {e}");
        }
    }

    /// Open a folder picker and start importing the selected folder.
    fn show_import_dialog(&self) {
        let window = match self.active_window() {
            Some(w) => w,
            None => return,
        };

        let file_dialog = gtk::FileDialog::builder()
            .title(gettext("Select Folder to Import"))
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

    /// Open the library on Tokio, then wire up the app on the GTK thread.
    ///
    /// On success: creates the client singletons, sets up the window's
    /// content, and starts the trash purge task and (for Immich) the sync
    /// engine. On failure: offers to re-run setup or quit.
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

                let import_mode = storage_mode.clone();
                let db = crate::library::db::Database::new();

                // Build Immich client + recorder + resolver based on config.
                // A client that fails to build must stop the open: falling
                // back to None would run an Immich library as local, with
                // no sync and no outbox.
                let immich_client = match immich_info
                    .as_ref()
                    .map(|(url, token)| {
                        crate::sync::providers::immich::client::ImmichClient::new(url, token)
                    })
                    .transpose()
                {
                    Ok(client) => client,
                    Err(e) => {
                        error!("failed to create Immich client: {e}");
                        app.show_open_failed_dialog(&window, &e);
                        return;
                    }
                };

                let recorder: Arc<dyn crate::library::recorder::MutationRecorder> =
                    if immich_client.is_some() {
                        Arc::new(crate::sync::outbox::QueueWriterOutbox::new(db.clone()))
                    } else {
                        Arc::new(crate::sync::outbox::NoOpRecorder)
                    };

                let resolver: Arc<dyn crate::library::resolver::OriginalResolver> =
                    if let Some(ref client) = immich_client {
                        Arc::new(
                            crate::sync::providers::immich::resolver::CachedResolver::new(
                                Arc::new(client.clone()),
                                originals_dir.clone(),
                            ),
                        )
                    } else {
                        Arc::new(crate::library::resolver::LocalResolver::new(
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

                        // Create the import client (GObject singleton).
                        let sync_thumbnails_dir = thumbnails_dir.clone();
                        {
                            let render_pipeline =
                                Arc::new(crate::renderer::pipeline::RenderPipeline::new());
                            *app.imp().render_pipeline.borrow_mut() =
                                Some(Arc::clone(&render_pipeline));

                            let import_client = crate::client::ImportClient::new();
                            import_client.configure(
                                Arc::clone(&library),
                                originals_dir,
                                thumbnails_dir,
                                Arc::clone(&render_pipeline),
                                storage_mode,
                                tokio.clone(),
                            );
                            app.set_import_client(import_client);
                        }

                        // Create the album client (GObject singleton).
                        // Subscribe to AlbumEvent for reactive model updates.
                        {
                            let albums_rx = library.albums().subscribe();
                            let album_client_v2 = crate::client::AlbumClientV2::new();
                            album_client_v2.configure(
                                Arc::clone(&library),
                                tokio.clone(),
                                albums_rx,
                            );
                            *app.imp().album_client_v2.borrow_mut() = Some(album_client_v2);
                        }

                        // Create the people client (GObject singleton).
                        // Subscribe to FacesEvent for reactive model updates.
                        {
                            let faces_rx = library.faces().subscribe();
                            let people_client = crate::client::PeopleClientV2::new();
                            people_client.configure(Arc::clone(&library), tokio.clone(), faces_rx);
                            *app.imp().people_client.borrow_mut() = Some(people_client);
                        }

                        // Create the MediaClient (GObject singleton).
                        // Subscribes to MediaEvent via the service's fan-out
                        // channel for reactive model updates.
                        {
                            let media_client_v2 = crate::client::MediaClientV2::new();
                            media_client_v2.configure(Arc::clone(&library), tokio.clone());
                            *app.imp().media_client_v2.borrow_mut() = Some(media_client_v2);
                        }

                        // Wire the shell: builds sidebar, registers views,
                        // and switches to the content page. Components react
                        // to mutations via per-service event channels and
                        // GObject signals on the client singletons.
                        let settings = app.settings();
                        window.setup(settings.clone());

                        // Start periodic trash purge task.
                        {
                            let retention_days = settings.uint("trash-retention-days");
                            let handle = crate::library::purge_trash::start(
                                Arc::clone(&library),
                                retention_days,
                                tokio.clone(),
                            );
                            *app.imp().purge_handle.borrow_mut() = Some(handle);
                        }

                        // Start Immich sync engine and sync client.
                        if let Some(client) = immich_client {
                            let sync_interval = u64::from(settings.uint("sync-interval-seconds"));

                            let (sync_events_tx, sync_events_rx) =
                                tokio::sync::mpsc::unbounded_channel();

                            let handle = crate::sync::SyncHandle::start(
                                client,
                                Arc::clone(&library),
                                db_for_sync.clone(),
                                sync_events_tx,
                                sync_thumbnails_dir,
                                sync_interval,
                                tokio.clone(),
                            );
                            *app.imp().sync_handle.borrow_mut() = Some(handle);

                            let sync_client = crate::client::SyncClient::new();
                            sync_client.configure(sync_events_rx, tokio.clone());
                            sync_client.set_outbox_repository(
                                crate::sync::outbox::OutboxRepository::new(db_for_sync),
                            );
                            app.set_sync_client(sync_client);
                        }
                    }
                    Err(e) => {
                        error!("failed to open library: {e}");
                        app.show_open_failed_dialog(&window, &e);
                    }
                }
            }
        ));
    }

    /// Tell the user the library could not be opened, offering to re-run
    /// setup (closing `window`) or quit.
    fn show_open_failed_dialog(&self, window: &MomentsWindow, err: &impl std::fmt::Display) {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Could not open library"))
            .body(with_details(
                &gettext("An error occurred while opening the library."),
                err,
            ))
            .build();
        dialog.add_response("setup", &gettext("Set Up Library"));
        dialog.add_response("quit", &gettext("Quit"));
        dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("setup"));
        dialog.set_close_response("setup");

        let app_weak = self.downgrade();
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

        dialog.present(Some(window));
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
fn with_details(message: &str, err: &impl std::fmt::Display) -> String {
    format!(
        "{message}\n\n{}",
        gettext("Details: {}").replace("{}", &err.to_string())
    )
}

/// For an Immich config, inject the session token from the keyring.
///
/// The bundle's `library.toml` doesn't hold the token, so the config read
/// from it has an empty `access_token`. Local configs pass through.
fn with_immich_token(config: LibraryConfig) -> Result<LibraryConfig, keyring::TokenError> {
    match config {
        LibraryConfig::Immich { server_url, .. } => {
            let access_token = keyring::resolve_immich_token(&server_url)?;
            Ok(LibraryConfig::Immich {
                server_url,
                access_token,
            })
        }
        other => Ok(other),
    }
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

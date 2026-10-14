// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

mod actions;
mod import;
pub mod keyring;
mod load;
mod open;

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use tracing::info;

use crate::config::APP_ID;
use crate::library::Library;

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
}

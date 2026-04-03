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

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::sync::{mpsc::Receiver, Arc};

use gettextrs::gettext;
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use tracing::{debug, error, info, instrument, warn};

use crate::app_event::AppEvent;
use crate::config::VERSION;
use crate::event_bus::EventBus;
use crate::library::bundle::Bundle;
use crate::library::config::LibraryConfig;
use crate::library::event::LibraryEvent;
use crate::library::factory::LibraryFactory;
use crate::library::Library;
use crate::ui::import_dialog::ImportDialog;
use crate::ui::MomentsSetupWindow;
use crate::ui::MomentsWindow;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MomentsApplication {
        pub settings: OnceCell<gio::Settings>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
        pub library: RefCell<Option<Arc<dyn Library>>>,
        pub is_immich: Cell<bool>,
        pub immich_server_url: RefCell<Option<String>>,
        pub library_events: RefCell<Option<Receiver<LibraryEvent>>>,
        /// Held while an import is in flight so the idle loop can update it.
        /// Cleared when `ImportComplete` arrives or the user dismisses it.
        pub import_dialog: RefCell<Option<ImportDialog>>,
        /// The GLib source ID of the library-event idle loop.
        ///
        /// Stored so `shutdown()` can remove it explicitly, which frees the
        /// closure and releases the `Rc<PhotoGridModel>` (→ `Arc<dyn Library>`
        /// → `SqlitePool`) before the Tokio runtime is dropped in `main()`.
        pub idle_source: RefCell<Option<glib::SourceId>>,
        /// Centralised event bus for fan-out event delivery.
        /// Created when the library is loaded.
        pub event_bus: RefCell<Option<EventBus>>,
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
            obj.set_accels_for_action("win.toggle-sidebar", &["F9"]);
            obj.set_accels_for_action("view.zoom-in", &["<control>equal", "<control>plus", "<control>KP_Add"]);
            obj.set_accels_for_action("view.zoom-out", &["<control>minus", "<control>KP_Subtract"]);
        }
    }

    impl ApplicationImpl for MomentsApplication {
        fn shutdown(&self) {
            info!("application shutting down");

            // Remove the idle source first — this frees the closure while
            // the Tokio runtime is still alive so the SqlitePool background
            // task can exit cleanly.
            if let Some(source_id) = self.idle_source.borrow_mut().take() {
                source_id.remove();
            }

            // Drop all library-related state so the Arc<dyn Library>
            // (and the SqlitePool it wraps) is freed before drop(tokio)
            // in main() tries to shut down the runtime.
            self.event_bus.borrow_mut().take();
            self.import_dialog.borrow_mut().take();
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
            let settings = self
                .settings
                .get_or_init(|| gio::Settings::new("io.github.justinf555.Moments"));
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

            let settings = self
                .settings
                .get_or_init(|| gio::Settings::new("io.github.justinf555.Moments"));

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

    /// Get the singleton application instance.
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
        self.add_action_entries([quit_action, about_action, import_action, preferences_action]);
    }

    fn show_about(&self) {
        let Some(window) = self.active_window() else { return };
        let about = adw::AboutDialog::builder()
            .application_name("moments")
            .application_icon("io.github.justinf555.Moments")
            .developer_name("Unknown")
            .version(VERSION)
            .developers(vec!["Unknown"])
            .translator_credits(&gettext("translator-credits"))
            .copyright("© 2026 Unknown")
            .build();

        about.present(Some(&window));
    }

    fn show_preferences(&self) {
        let window = match self.active_window() {
            Some(w) => w,
            None => return,
        };
        let settings = self.imp().settings.get().expect("settings initialised").clone();
        let is_immich = self.imp().is_immich.get();
        let library = self.imp().library.borrow().clone();
        let immich_url = self.imp().immich_server_url.borrow().clone();

        crate::ui::preferences_dialog::show_preferences(
            &window,
            &settings,
            is_immich,
            library,
            immich_url,
        );
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

        // Determine config from the bundle manifest (works for both local and Immich).
        // For new bundles, we create first then open to read the config back.
        // For Immich, the ImmichSetupPage already called Bundle::create before emitting
        // setup-complete. For Local, we create here.
        let (bundle, config) = if bundle_path.exists() {
            // Immich path: bundle was created by ImmichSetupPage.
            match Bundle::open(&bundle_path) {
                Ok(result) => result,
                Err(e) => {
                    error!("failed to open bundle: {e}");
                    show_library_error_dialog(
                        setup_win,
                        "Could not open library",
                        &format!("The library at {} could not be opened.\n\nDetails: {e}", bundle_path.display()),
                    );
                    return;
                }
            }
        } else {
            // Local path: create the bundle now.
            let bundle = match Bundle::create(&bundle_path, &LibraryConfig::Local) {
                Ok(b) => b,
                Err(e) => {
                    error!("failed to create library bundle: {e}");
                    show_library_error_dialog(
                        setup_win,
                        "Could not create library",
                        &format!("Failed to create a library at {}.\n\nDetails: {e}", bundle_path.display()),
                    );
                    return;
                }
            };
            (bundle, LibraryConfig::Local)
        };

        // For Immich configs, inject the session token from the keyring.
        let config = match config {
            LibraryConfig::Immich { server_url, .. } => {
                let access_token = crate::library::keyring::lookup_access_token(&server_url)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                LibraryConfig::Immich { server_url, access_token }
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
                let access_token = crate::library::keyring::lookup_access_token(&server_url)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                LibraryConfig::Immich { server_url, access_token }
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
                move |result| match result {
                    Ok(folder) => {
                        app.run_import(folder);
                    }
                    Err(_) => {} // user cancelled — nothing to do
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
        let library = match self.imp().library.borrow().clone() {
            Some(l) => l,
            None => {
                error!("import requested but no library is open");
                return;
            }
        };
        let tokio = self.imp().tokio.get().expect("tokio handle set").clone();
        let window = match self.active_window() {
            Some(w) => w,
            None => return,
        };

        let display_path = folder.path().map(|p| p.display().to_string())
            .unwrap_or_else(|| folder.uri().to_string());
        info!(path = %display_path, "starting import");

        // Resolve folder contents via GIO to handle Flatpak portal paths.
        let sources = resolve_folder_via_gio(&folder);
        if sources.is_empty() {
            warn!(path = %display_path, "no files found in folder");
            return;
        }
        debug!(count = sources.len(), "resolved import sources via GIO");

        let win_weak = window.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.import(sources).await })
                .await;
            if let Ok(Err(e)) = result {
                error!("import pipeline error: {e}");
                if let Some(win) = win_weak.upgrade() {
                    let _ = win.activate_action("win.show-toast", Some(&"Import failed".to_variant()));
                }
            }
        });
    }

    /// Spawn the async factory call on the glib main context.
    ///
    /// On success:
    ///  1. Creates a `PhotoGridModel` backed by the new library.
    ///  2. Wires the model into the window's photo grid.
    ///  3. Switches the window to its content page.
    ///  4. Starts polling `LibraryEvent`s via `glib::idle_add_local`, forwarding
    ///     `ThumbnailReady` events into the model so cells repaint automatically.
    fn load_library_async(&self, bundle: Bundle, config: LibraryConfig, window: MomentsWindow) {
        // Store backend type for preferences dialog.
        if let LibraryConfig::Immich { ref server_url, .. } = config {
            self.imp().is_immich.set(true);
            *self.imp().immich_server_url.borrow_mut() = Some(server_url.clone());
        }

        let (sender, receiver) = std::sync::mpsc::channel::<LibraryEvent>();
        *self.imp().library_events.borrow_mut() = Some(receiver);

        glib::MainContext::default().spawn_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            window,
            async move {
                let tokio = app.imp().tokio.get().expect("tokio handle set").clone();
                match LibraryFactory::create(bundle, config, sender, tokio.clone()).await {
                    Ok(library) => {
                        info!("library ready");

                        // Store library on the application.
                        *app.imp().library.borrow_mut() = Some(Arc::clone(&library));

                        // Create the event bus before wiring the shell so
                        // components can subscribe during construction.
                        let bus = EventBus::new();
                        let bus_tx = bus.sender();

                        // Wire the shell: builds sidebar, registers views,
                        // and switches to the content page. All components
                        // subscribe to the bus for event delivery.
                        let settings = app.imp().settings.get()
                            .expect("settings initialised").clone();
                        window.setup(library, tokio.clone(), settings, &bus);

                        // Create the command dispatcher — routes *Requested
                        // events to library calls on the Tokio runtime.
                        // The dispatcher is a unit struct; the real work is the
                        // subscriber closure registered in new(), which lives in
                        // the bus's thread-local SUBSCRIBERS for the app lifetime.
                        let _dispatcher = crate::commands::dispatcher::CommandDispatcher::new(
                            Arc::clone(app.imp().library.borrow().as_ref().unwrap()),
                            tokio.clone(),
                            &bus,
                        );

                        // Subscribe for error toasts — centralised error
                        // handling for all command failures.
                        {
                            let win_weak = window.downgrade();
                            bus.subscribe(move |event| {
                                if let AppEvent::Error(msg) = event {
                                    if let Some(win) = win_weak.upgrade() {
                                        gtk::prelude::WidgetExt::activate_action(
                                            &win,
                                            "win.show-toast",
                                            Some(&msg.to_variant()),
                                        ).ok();
                                    }
                                }
                            });
                        }

                        // Store bus for shutdown cleanup.
                        *app.imp().event_bus.borrow_mut() = Some(bus);

                        // Poll library events on every GTK idle tick.
                        // Routes thumbnail and import events via the registry.
                        let receiver = app
                            .imp()
                            .library_events
                            .borrow_mut()
                            .take()
                            .expect("receiver set above");

                        // ── Event translator ─────────────────────────────
                        // Thin 1:1 mapping from LibraryEvent → AppEvent.
                        // The idle loop is now a pure translator — no routing
                        // logic, no references to models, sidebar, or dialogs.
                        // Subscribers handle their own events via the bus.
                        let app_for_idle = app.downgrade();
                        let win_for_idle = window.downgrade();
                        let source_id = glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                            let app = match app_for_idle.upgrade() {
                                Some(a) => a,
                                None => return glib::ControlFlow::Break,
                            };
                            loop {
                                match receiver.try_recv() {
                                    Ok(LibraryEvent::ThumbnailReady { media_id }) => {
                                        bus_tx.send(AppEvent::ThumbnailReady { media_id });
                                    }
                                    Ok(LibraryEvent::ImportProgress { current, total, imported, skipped, failed }) => {
                                        // Import dialog is app-level (not a bus subscriber).
                                        let borrow = app.imp().import_dialog.borrow();
                                        if let Some(d) = borrow.as_ref() {
                                            d.set_progress(current, total);
                                        }
                                        bus_tx.send(AppEvent::ImportProgress { current, total, imported, skipped, failed });
                                    }
                                    Ok(LibraryEvent::ImportComplete(summary)) => {
                                        // Import dialog is app-level (not a bus subscriber).
                                        {
                                            let borrow = app.imp().import_dialog.borrow();
                                            if let Some(d) = borrow.as_ref() {
                                                d.set_complete(&summary);
                                            }
                                        }
                                        app.imp().import_dialog.borrow_mut().take();
                                        bus_tx.send(AppEvent::ImportComplete { summary });
                                        // Navigate to Recent Imports so the user sees what arrived.
                                        if let Some(win) = win_for_idle.upgrade() {
                                            win.navigate("recent");
                                        }
                                    }
                                    Ok(LibraryEvent::AssetSynced { item }) => {
                                        bus_tx.send(AppEvent::AssetSynced { item });
                                    }
                                    Ok(LibraryEvent::AssetDeletedRemote { media_id }) => {
                                        bus_tx.send(AppEvent::AssetDeletedRemote { media_id });
                                    }
                                    Ok(LibraryEvent::AlbumCreated { id, name }) => {
                                        bus_tx.send(AppEvent::AlbumCreated { id, name });
                                    }
                                    Ok(LibraryEvent::AlbumDeleted { id }) => {
                                        // NOTE: coordinator cleanup is synchronous and intentionally
                                        // precedes the bus dispatch — the route must be dead before
                                        // the sidebar removes the entry, to avoid a navigation race.
                                        if let Some(win) = win_for_idle.upgrade() {
                                            if let Some(coord) = win.imp().coordinator.get() {
                                                let route = format!("album:{}", id.as_str());
                                                coord.borrow_mut().unregister(&route);
                                            }
                                        }
                                        bus_tx.send(AppEvent::AlbumDeleted { id });
                                    }
                                    Ok(LibraryEvent::AlbumRenamed { id, name }) => {
                                        bus_tx.send(AppEvent::AlbumRenamed { id, name });
                                    }
                                    Ok(LibraryEvent::AlbumMediaChanged { album_id }) => {
                                        bus_tx.send(AppEvent::AlbumMediaChanged { album_id });
                                    }
                                    Ok(LibraryEvent::PeopleSyncComplete) => {
                                        bus_tx.send(AppEvent::PeopleSyncComplete);
                                        if let Some(win) = win_for_idle.upgrade() {
                                            win.reload_people();
                                        }
                                    }
                                    Ok(LibraryEvent::SyncStarted) => {
                                        bus_tx.send(AppEvent::SyncStarted);
                                    }
                                    Ok(LibraryEvent::SyncProgress { assets, people, faces }) => {
                                        bus_tx.send(AppEvent::SyncProgress { assets, people, faces });
                                    }
                                    Ok(LibraryEvent::SyncComplete { assets, people, faces, errors }) => {
                                        bus_tx.send(AppEvent::SyncComplete { assets, people, faces, errors });
                                    }
                                    Ok(LibraryEvent::ThumbnailDownloadProgress { completed, total }) => {
                                        bus_tx.send(AppEvent::ThumbnailDownloadProgress { completed, total });
                                    }
                                    Ok(LibraryEvent::ThumbnailDownloadsComplete { total }) => {
                                        bus_tx.send(AppEvent::ThumbnailDownloadsComplete { total });
                                    }
                                    Ok(_) => {}
                                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                        return glib::ControlFlow::Break;
                                    }
                                }
                            }
                            glib::ControlFlow::Continue
                        });
                        *app.imp().idle_source.borrow_mut() = Some(source_id);
                    }
                    Err(e) => {
                        error!("failed to open library: {e}");

                        let dialog = adw::AlertDialog::builder()
                            .heading("Could not open library")
                            .body(&format!(
                                "An error occurred while opening the library.\n\nDetails: {e}"
                            ))
                            .build();
                        dialog.add_response("setup", "Set Up Library");
                        dialog.add_response("quit", "Quit");
                        dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
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

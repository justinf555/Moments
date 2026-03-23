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

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc::Receiver, Arc};

use gettextrs::gettext;
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use tracing::{error, info, instrument};

use crate::config::VERSION;
use crate::library::bundle::Bundle;
use crate::library::config::LibraryConfig;
use crate::library::event::LibraryEvent;
use crate::library::factory::LibraryFactory;
use crate::library::Library;
use crate::ui::import_dialog::ImportDialog;
use crate::ui::photo_grid::PhotoGridModel;
use crate::ui::MomentsSetupWindow;
use crate::ui::MomentsWindow;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MomentsApplication {
        pub settings: OnceCell<gio::Settings>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
        pub library: RefCell<Option<Arc<dyn Library>>>,
        pub library_events: RefCell<Option<Receiver<LibraryEvent>>>,
        pub photo_grid_model: RefCell<Option<Rc<PhotoGridModel>>>,
        /// Held while an import is in flight so the idle loop can update it.
        /// Cleared when `ImportComplete` arrives or the user dismisses it.
        pub import_dialog: RefCell<Option<ImportDialog>>,
        /// The GLib source ID of the library-event idle loop.
        ///
        /// Stored so `shutdown()` can remove it explicitly, which frees the
        /// closure and releases the `Rc<PhotoGridModel>` (→ `Arc<dyn Library>`
        /// → `SqlitePool`) before the Tokio runtime is dropped in `main()`.
        pub idle_source: RefCell<Option<glib::SourceId>>,
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

            // Remove the idle source first — this frees the closure and
            // releases Rc<PhotoGridModel> while the Tokio runtime is still
            // alive so the SqlitePool background task can exit cleanly.
            if let Some(source_id) = self.idle_source.borrow_mut().take() {
                source_id.remove();
            }

            // Drop all library-related state so the Arc<dyn Library>
            // (and the SqlitePool it wraps) is freed before drop(tokio)
            // in main() tries to shut down the runtime.
            self.photo_grid_model.borrow_mut().take();
            self.import_dialog.borrow_mut().take();
            self.library.borrow_mut().take();

            self.parent_shutdown();
        }

        fn activate(&self) {
            let app = self.obj();

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
        self.add_action_entries([quit_action, about_action, import_action]);
    }

    fn show_about(&self) {
        let window = self.active_window().unwrap();
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

    /// Show the first-run setup window.
    fn show_setup_window(&self) {
        let setup = MomentsSetupWindow::new(self);
        setup.connect_setup_complete(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |win, path| {
                app.on_setup_complete(win, path);
            }
        ));
        setup.present();
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

        let bundle = match Bundle::create(&bundle_path, &LibraryConfig::Local) {
            Ok(b) => b,
            Err(e) => {
                error!("failed to create library bundle: {e}");
                return;
            }
        };

        let settings = self.imp().settings.get().expect("settings initialised");
        if let Err(e) = settings.set_string("library-path", &path) {
            error!("failed to save library path to GSettings: {e}");
        }

        // Present the main window first, then close setup — ensures there is
        // always at least one window alive during the transition.
        let window = MomentsWindow::new(self);
        window.present();
        setup_win.close();

        self.load_library_async(bundle, LibraryConfig::Local, window);
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
                self.show_setup_window();
                return;
            }
        };

        let window = MomentsWindow::new(self);
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
                        if let Some(path) = folder.path() {
                            app.run_import(path);
                        }
                    }
                    Err(_) => {} // user cancelled — nothing to do
                }
            ),
        );
    }

    /// Create the import progress dialog and kick off the import pipeline.
    fn run_import(&self, folder: PathBuf) {
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

        let dialog = ImportDialog::new();

        // Clear our reference when the user dismisses the dialog early so the
        // idle loop stops trying to forward events to a closed widget.
        dialog.connect_closed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| {
                app.imp().import_dialog.borrow_mut().take();
            }
        ));

        dialog.present(Some(&window));
        *self.imp().import_dialog.borrow_mut() = Some(dialog);

        info!(path = %folder.display(), "starting import");
        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { library.import(vec![folder]).await })
                .await;
            if let Ok(Err(e)) = result {
                error!("import pipeline error: {e}");
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

                        let model = Rc::new(PhotoGridModel::new(Arc::clone(&library), tokio.clone()));

                        // Store library and model on the application.
                        *app.imp().library.borrow_mut() = Some(Arc::clone(&library));
                        *app.imp().photo_grid_model.borrow_mut() = Some(Rc::clone(&model));

                        // Wire the shell: builds sidebar, registers views,
                        // and switches to the content page.
                        let settings = app.imp().settings.get()
                            .expect("settings initialised").clone();
                        window.setup(Rc::clone(&model), library, tokio.clone(), settings);

                        // Poll library events on every GTK idle tick.
                        // Routes thumbnail and import events to the right consumers.
                        let receiver = app
                            .imp()
                            .library_events
                            .borrow_mut()
                            .take()
                            .expect("receiver set above");

                        let app_for_idle = app.downgrade();
                        let source_id = glib::idle_add_local(move || {
                            let app = match app_for_idle.upgrade() {
                                Some(a) => a,
                                None => return glib::ControlFlow::Break,
                            };
                            loop {
                                match receiver.try_recv() {
                                    Ok(LibraryEvent::ThumbnailReady { media_id }) => {
                                        model.on_thumbnail_ready(&media_id);
                                    }
                                    Ok(LibraryEvent::ImportProgress { current, total }) => {
                                        let borrow = app.imp().import_dialog.borrow();
                                        if let Some(d) = borrow.as_ref() {
                                            d.set_progress(current, total);
                                        }
                                    }
                                    Ok(LibraryEvent::ImportComplete(summary)) => {
                                        {
                                            let borrow = app.imp().import_dialog.borrow();
                                            if let Some(d) = borrow.as_ref() {
                                                d.set_complete(&summary);
                                            }
                                        }
                                        // Release strong ref — dialog stays open until user dismisses.
                                        app.imp().import_dialog.borrow_mut().take();
                                        model.reload();
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
                    }
                }
            }
        ));
    }
}

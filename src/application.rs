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
        }
    }

    impl ApplicationImpl for MomentsApplication {
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
        self.add_action_entries([quit_action, about_action]);
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
    fn open_library(&self, path: PathBuf) {
        let (bundle, config) = match Bundle::open(&path) {
            Ok(result) => result,
            Err(e) => {
                error!("failed to open library bundle: {e}");
                return;
            }
        };

        let window = MomentsWindow::new(self);
        window.present();

        self.load_library_async(bundle, config, window);
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

                        let model = Rc::new(PhotoGridModel::new(Arc::clone(&library), tokio));

                        // Store library and model on the application.
                        *app.imp().library.borrow_mut() = Some(library);
                        *app.imp().photo_grid_model.borrow_mut() = Some(Rc::clone(&model));

                        // Wire the grid before revealing the content page.
                        window.set_model(Rc::clone(&model));
                        window.set_library_ready();

                        // Poll library events and forward thumbnail notifications.
                        let receiver = app
                            .imp()
                            .library_events
                            .borrow_mut()
                            .take()
                            .expect("receiver set above");

                        glib::idle_add_local(move || {
                            loop {
                                match receiver.try_recv() {
                                    Ok(LibraryEvent::ThumbnailReady { media_id }) => {
                                        model.on_thumbnail_ready(&media_id);
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
                    }
                    Err(e) => {
                        error!("failed to open library: {e}");
                    }
                }
            }
        ));
    }
}

/* window.rs
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

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use tracing::debug;

use crate::library::Library;

use crate::ui::coordinator::ContentCoordinator;
use crate::ui::empty_library::EmptyLibraryView;
use crate::ui::model_registry::ModelRegistry;
use crate::ui::photo_grid::{PhotoGridModel, PhotoGridView};
use crate::ui::sidebar::MomentsSidebar;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/window.ui")]
    pub struct MomentsWindow {
        #[template_child]
        pub main_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub split_view: TemplateChild<adw::NavigationSplitView>,

        /// Set up once in `setup()` — holds live references to all registered views.
        pub coordinator: OnceCell<Rc<RefCell<ContentCoordinator>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsWindow {
        const NAME: &'static str = "MomentsWindow";
        type Type = super::MomentsWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MomentsWindow {}
    impl WidgetImpl for MomentsWindow {}
    impl WindowImpl for MomentsWindow {}
    impl ApplicationWindowImpl for MomentsWindow {}
    impl AdwApplicationWindowImpl for MomentsWindow {}
}

glib::wrapper! {
    pub struct MomentsWindow(ObjectSubclass<imp::MomentsWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl MomentsWindow {
    pub fn new<P: IsA<gtk::Application>>(application: &P) -> Self {
        glib::Object::builder()
            .property("application", application)
            .build()
    }

    /// Wire the library model into the shell and switch to the content page.
    ///
    /// Builds the sidebar, registers all content views with the coordinator,
    /// then switches `main_stack` from "loading" to "content".
    /// Wire the library into the shell and switch to the content page.
    ///
    /// Photos is created eagerly (always the default view). Other routes
    /// are registered lazily — their views are materialised on first
    /// navigation. Returns a [`ModelRegistry`] so the caller can forward
    /// library events to all models (including those created later).
    pub fn setup(
        &self,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        settings: gio::Settings,
    ) -> Rc<ModelRegistry> {
        let imp = self.imp();
        use crate::library::media::MediaFilter;

        let registry = ModelRegistry::new();

        // Build sidebar — MomentsSidebar is already an AdwNavigationPage subclass.
        let sidebar = MomentsSidebar::new();
        imp.split_view.set_sidebar(Some(&sidebar));

        // Build content stack + coordinator.
        let content_stack = gtk::Stack::new();
        content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        let mut coordinator = ContentCoordinator::new(content_stack.clone());

        // Register the empty-library view (eager, no model).
        coordinator.register("empty", Rc::new(EmptyLibraryView::new()));

        // Register the Photos view (eager — always the default).
        let photos_model = Rc::new(PhotoGridModel::new(
            Arc::clone(&library),
            tokio.clone(),
            MediaFilter::All,
        ));
        let photos_view = Rc::new(PhotoGridView::new(
            Arc::clone(&library),
            tokio.clone(),
            settings.clone(),
        ));
        photos_view.set_model(Rc::clone(&photos_model));
        self.insert_action_group("view", Some(photos_view.view_actions()));
        registry.register(&photos_model);
        coordinator.register("photos", photos_view);

        // Register the Favorites view (lazy — created on first click).
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings;
            let reg = Rc::clone(&registry);
            coordinator.register_lazy("favorites", move || {
                let model = Rc::new(PhotoGridModel::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    MediaFilter::Favorites,
                ));
                let view = Rc::new(PhotoGridView::new(lib, tk, s));
                view.set_model(Rc::clone(&model));
                reg.register(&model);
                view
            });
        }

        // Wrap the content stack in a NavigationPage for the split view.
        let content_nav_page = adw::NavigationPage::builder()
            .title("Photos")
            .child(&content_stack)
            .build();
        imp.split_view.set_content(Some(&content_nav_page));

        let coordinator = Rc::new(RefCell::new(coordinator));

        // Start on "empty" — items-changed will switch to "photos" once
        // the first page arrives.
        coordinator.borrow_mut().navigate("empty");

        // Toggle between empty and content based on store item count.
        // Connected to the photos store (the default view).
        {
            let stack = content_stack.clone();
            photos_model.store.connect_items_changed(move |store, _, _, _| {
                let target = if store.n_items() > 0 { "photos" } else { "empty" };
                stack.set_visible_child_name(target);
            });
        }

        imp.coordinator
            .set(coordinator)
            .expect("coordinator set once in setup()");

        // Wire sidebar selection → coordinator navigation.
        let obj_weak = self.downgrade();
        sidebar.connect_route_selected(move |id| {
            let Some(win) = obj_weak.upgrade() else { return };
            if let Some(coordinator) = win.imp().coordinator.get() {
                coordinator.borrow_mut().navigate(id);
            }
        });

        sidebar.select_first();

        // Add the `win.toggle-sidebar` stateful action.
        self.install_toggle_sidebar_action();

        debug!("switching main window to content page");
        imp.main_stack.set_visible_child_name("content");

        registry
    }

    /// Install a `win.toggle-sidebar` boolean action wired to the split view.
    ///
    /// In collapsed (narrow) mode, toggles between showing the sidebar and
    /// the content page. In wide mode the split view always shows both and
    /// the action is a no-op.
    fn install_toggle_sidebar_action(&self) {
        let split_view = self.imp().split_view.get();

        // In collapsed mode, `shows_content()` tells us which pane is visible.
        // We start with the sidebar visible (content hidden).
        let state = false.to_variant(); // sidebar is visible by default
        let action = gio::SimpleAction::new_stateful("toggle-sidebar", None, &state);

        let split_weak = split_view.downgrade();
        action.connect_activate(move |act, _| {
            let Some(sv) = split_weak.upgrade() else { return };
            if sv.is_collapsed() {
                let show_content = !sv.shows_content();
                sv.set_show_content(show_content);
                act.set_state(&(!show_content).to_variant()); // state = sidebar visible
            }
        });

        self.add_action(&action);
    }
}

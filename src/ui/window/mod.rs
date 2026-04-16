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

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{gio, glib};
use tracing::{debug, instrument, warn};

use crate::ui::coordinator::ContentCoordinator;
use crate::ui::empty_library::EmptyLibraryView;
use crate::ui::people_grid::PeopleGridView;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;
use crate::ui::sidebar::MomentsSidebar;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/window/window.ui")]
    pub struct MomentsWindow {
        #[template_child]
        pub main_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub split_view: TemplateChild<adw::NavigationSplitView>,

        /// Set up once in `setup()` — holds live references to all registered views.
        pub coordinator: OnceCell<Rc<RefCell<ContentCoordinator>>>,

        /// Sidebar reference for event-driven album updates.
        pub sidebar: OnceCell<MomentsSidebar>,

        /// GSettings instance for persisting window geometry.
        pub settings: OnceCell<gio::Settings>,

        /// Event bus subscriptions kept alive for the window's lifetime.
        pub _subscriptions: RefCell<Vec<crate::event_bus::Subscription>>,
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
    impl WindowImpl for MomentsWindow {
        fn close_request(&self) -> glib::Propagation {
            if let Some(settings) = self.settings.get() {
                let win = self.obj();
                let is_maximized = win.is_maximized();
                if let Err(e) = settings.set_boolean("is-maximized", is_maximized) {
                    tracing::warn!("failed to save is-maximized: {e}");
                }

                // Only save dimensions when not maximized, so we preserve
                // the pre-maximized size for next launch.
                if !is_maximized {
                    let (width, height) = win.default_size();
                    if let Err(e) = settings.set_int("window-width", width) {
                        tracing::warn!("failed to save window-width: {e}");
                    }
                    if let Err(e) = settings.set_int("window-height", height) {
                        tracing::warn!("failed to save window-height: {e}");
                    }
                }
                debug!(is_maximized, "saved window state on close");
            }
            self.parent_close_request()
        }
    }
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
    pub fn new<P: IsA<gtk::Application>>(application: &P, settings: &gio::Settings) -> Self {
        let win: Self = glib::Object::builder()
            .property("application", application)
            .build();
        win.restore_window_state(settings);

        // Development builds get the GNOME "devel" style (striped headerbar)
        // and a title suffix so the user can tell them apart from production.
        if crate::config::PROFILE == "development" {
            win.add_css_class("devel");
        }

        win
    }

    /// Restore window size and maximized state from GSettings.
    #[instrument(skip(self, settings))]
    fn restore_window_state(&self, settings: &gio::Settings) {
        let width = settings.int("window-width");
        let height = settings.int("window-height");
        let is_maximized = settings.boolean("is-maximized");

        self.set_default_size(width, height);
        if is_maximized {
            self.maximize();
        }

        self.imp()
            .settings
            .set(settings.clone())
            .expect("settings set once in restore_window_state");

        debug!(width, height, is_maximized, "restored window state");
    }

    /// Wire the full shell: sidebar, coordinator, views.
    ///
    /// All models subscribe to the [`EventBus`] for event delivery.
    /// The caller does not need to forward events — components are
    /// self-contained.
    pub fn setup(&self, settings: gio::Settings, bus: &crate::event_bus::EventBus) {
        let imp = self.imp();
        let bus_sender = bus.sender();

        let sidebar = self.setup_sidebar();

        let texture_cache = Rc::new(TextureCache::new());

        let (content_stack, coordinator, photos_model) =
            self.build_coordinator(&settings, &texture_cache, &bus_sender);

        self.register_lazy_views(
            &mut coordinator.borrow_mut(),
            &settings,
            &texture_cache,
            &bus_sender,
        );

        let content_nav_page = adw::NavigationPage::builder()
            .title("Photos")
            .child(&content_stack)
            .build();
        imp.split_view.set_content(Some(&content_nav_page));

        coordinator.borrow_mut().navigate("empty");

        Self::connect_empty_toggle(&content_stack, &photos_model);

        imp.coordinator
            .set(Rc::clone(&coordinator))
            .expect("coordinator set once in setup()");

        self.connect_sidebar_navigation(&sidebar, &settings, &texture_cache, &bus_sender);

        sidebar.select_first();

        // Explicitly navigate to "photos" and install view actions.
        // AdwSidebar::set_selected() does not emit `activated` (only user
        // interactions do), so the sidebar callback that installs zoom
        // actions won't fire on startup.
        self.navigate("photos");

        self.install_show_toast_action();
        self.install_toggle_sidebar_action();
        self.subscribe_bus_events(bus);

        debug!("switching main window to content page");
        imp.main_stack.set_visible_child_name("content");
    }

    /// Subscribe to event bus events that the window handles directly.
    fn subscribe_bus_events(&self, bus: &crate::event_bus::EventBus) {
        use crate::app_event::AppEvent;
        let mut subs = self.imp()._subscriptions.borrow_mut();

        // Navigate to Recent Imports when an import completes.
        // Deferred via idle_add_local_once because navigate() can materialise
        // a lazy view, which triggers realize → subscribe() on the new widget.
        if let Some(import_client) =
            crate::application::MomentsApplication::default().import_client()
        {
            let weak = self.downgrade();
            import_client.connect_notify_local(Some("state"), move |client, _| {
                if client.state() == crate::client::ImportState::Complete {
                    let weak = weak.clone();
                    glib::idle_add_local_once(move || {
                        if let Some(win) = weak.upgrade() {
                            win.navigate("recent");
                        }
                    });
                }
            });
        }

        // Unregister deleted album routes from the coordinator before
        // AlbumGridView processes the event (avoids a navigation race).
        let weak = self.downgrade();
        subs.push(bus.subscribe(move |event| {
            if let AppEvent::AlbumDeleted { id } = event {
                if let Some(win) = weak.upgrade() {
                    if let Some(coord) = win.imp().coordinator.get() {
                        let route = format!("album:{}", id.as_str());
                        coord.borrow_mut().unregister(&route);
                    }
                }
            }
        }));
    }

    fn setup_sidebar(&self) -> MomentsSidebar {
        let imp = self.imp();

        let sidebar = MomentsSidebar::new();

        // Hide People route for Local backend (no face detection).
        let app = crate::application::MomentsApplication::default();
        if !app.imp().is_immich.get() {
            sidebar.hide_people();
        }

        imp.split_view.set_sidebar(Some(&sidebar));
        imp.sidebar
            .set(sidebar.clone())
            .expect("sidebar set once in setup()");

        {
            let sb = sidebar.clone();
            let media_client = crate::application::MomentsApplication::default()
                .media_client()
                .expect("media client available");
            media_client.library_stats(move |result| {
                if let Ok(stats) = result {
                    sb.set_trash_count(stats.trashed_count as u32);
                }
            });
        }

        sidebar.setup_pinned_albums();

        sidebar
    }

    #[allow(clippy::type_complexity)]
    fn build_coordinator(
        &self,
        settings: &gio::Settings,
        texture_cache: &Rc<TextureCache>,
        bus_sender: &crate::event_bus::EventSender,
    ) -> (gtk::Stack, Rc<RefCell<ContentCoordinator>>, gio::ListStore) {
        use crate::library::media::MediaFilter;

        let media_client = crate::application::MomentsApplication::default()
            .media_client()
            .expect("media client available");

        let content_stack = gtk::Stack::new();
        content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        let mut coordinator = ContentCoordinator::new(content_stack.clone());

        let empty = EmptyLibraryView::new();
        coordinator.register("empty", empty.widget());

        let photos_store = media_client.create_model(MediaFilter::All);
        let photos_view = PhotoGridView::new();
        photos_view.setup(
            settings.clone(),
            Rc::clone(texture_cache),
            bus_sender.clone(),
        );
        photos_view.set_store(photos_store.clone(), MediaFilter::All);
        coordinator.register("photos", &photos_view);

        (
            content_stack,
            Rc::new(RefCell::new(coordinator)),
            photos_store,
        )
    }

    fn register_lazy_views(
        &self,
        coordinator: &mut ContentCoordinator,
        settings: &gio::Settings,
        texture_cache: &Rc<TextureCache>,
        bus_sender: &crate::event_bus::EventSender,
    ) {
        use crate::library::media::MediaFilter;

        {
            let s = settings.clone();
            let tc = Rc::clone(texture_cache);
            let bs = bus_sender.clone();
            coordinator.register_lazy("favorites", move || {
                let mc = crate::application::MomentsApplication::default()
                    .media_client()
                    .expect("media client available");
                let store = mc.create_model(MediaFilter::Favorites);
                let view = PhotoGridView::new();
                view.setup(s, tc, bs);
                view.set_store(store, MediaFilter::Favorites);
                view.upcast()
            });
        }

        {
            let s = settings.clone();
            let tc = Rc::clone(texture_cache);
            let bs = bus_sender.clone();
            coordinator.register_lazy("recent", move || {
                let days = s.uint("recent-imports-days") as i64;
                let since = chrono::Utc::now().timestamp() - days * 86400;
                let filter = MediaFilter::RecentImports { since };
                let mc = crate::application::MomentsApplication::default()
                    .media_client()
                    .expect("media client available");
                let store = mc.create_model(filter.clone());
                let view = PhotoGridView::new();
                view.setup(s, tc, bs);
                view.set_store(store, filter);
                view.upcast()
            });
        }

        {
            let s = settings.clone();
            let tc = Rc::clone(texture_cache);
            let bs = bus_sender.clone();
            coordinator.register_lazy("trash", move || {
                let mc = crate::application::MomentsApplication::default()
                    .media_client()
                    .expect("media client available");
                let store = mc.create_model(MediaFilter::Trashed);
                let view = PhotoGridView::new();
                view.setup(s, tc, bs);
                view.set_store(store, MediaFilter::Trashed);
                view.upcast()
            });
        }

        {
            let s = settings.clone();
            let tc = Rc::clone(texture_cache);
            let bs = bus_sender.clone();
            coordinator.register_lazy("people", move || {
                let view = PeopleGridView::new();
                view.setup_people(s, tc, bs);
                view.upcast()
            });
        }

        {
            let s = settings.clone();
            let tc = Rc::clone(texture_cache);
            let bs = bus_sender.clone();
            coordinator.register_lazy("albums", move || {
                let view = super::album_grid::AlbumGridView::new();
                view.setup(s, tc, bs);
                view.upcast()
            });
        }
    }

    /// Wire the empty ↔ photos stack toggle based on store item count.
    ///
    /// Only switches on empty ↔ non-empty transitions — deliberately does NOT
    /// override the visible child if the user has navigated away from Photos
    /// (e.g. to Trash).
    fn connect_empty_toggle(content_stack: &gtk::Stack, photos_store: &gio::ListStore) {
        let stack = content_stack.clone();
        let was_empty = std::cell::Cell::new(true);
        photos_store.connect_items_changed(move |store, _, _, _| {
            let is_empty = store.n_items() == 0;
            if is_empty && !was_empty.get() {
                stack.set_visible_child_name("empty");
                was_empty.set(true);
            } else if !is_empty && was_empty.get() {
                stack.set_visible_child_name("photos");
                was_empty.set(false);
            }
        });
    }

    fn connect_sidebar_navigation(
        &self,
        sidebar: &MomentsSidebar,
        settings: &gio::Settings,
        texture_cache: &Rc<TextureCache>,
        bus_sender: &crate::event_bus::EventSender,
    ) {
        let obj_weak = self.downgrade();
        let s = settings.clone();
        let tc = Rc::clone(texture_cache);
        let bs = bus_sender.clone();
        sidebar.connect_route_selected(move |id| {
            let Some(win) = obj_weak.upgrade() else {
                return;
            };
            let Some(coordinator) = win.imp().coordinator.get() else {
                return;
            };

            if let Some(album_id_str) = id.strip_prefix("album:") {
                let mut coord = coordinator.borrow_mut();
                if !coord.has_route(id) {
                    use crate::library::album::AlbumId;
                    use crate::library::media::MediaFilter;
                    let album_id = AlbumId::from_raw(album_id_str.to_owned());
                    let filter = MediaFilter::Album { album_id };
                    let mc = crate::application::MomentsApplication::default()
                        .media_client()
                        .expect("media client available");
                    let store = mc.create_model(filter.clone());
                    let view = PhotoGridView::new();
                    view.setup(s.clone(), Rc::clone(&tc), bs.clone());
                    view.set_store(store, filter);
                    coord.register(id, &view);
                }
                coord.navigate(id);
            } else {
                coordinator.borrow_mut().navigate(id);
            }
        });
    }

    /// Access the sidebar for event-driven album updates.
    pub fn sidebar(&self) -> Option<&MomentsSidebar> {
        self.imp().sidebar.get()
    }

    /// Navigate to the given route by id (e.g. "recent", "photos").
    pub fn navigate(&self, route_id: &str) {
        if let Some(coordinator) = self.imp().coordinator.get() {
            coordinator.borrow_mut().navigate(route_id);
        }
    }

    /// Reload the People collection grid if it has been materialised.
    /// Show a toast message in the window's toast overlay.
    ///
    /// Auto-dismisses after 5 seconds.
    pub fn show_toast(&self, message: &str) {
        let toast = adw::Toast::new(message);
        toast.set_timeout(5);
        self.imp().toast_overlay.add_toast(toast);
    }

    /// Install the `win.show-toast` action (string parameter).
    ///
    /// Any widget in the window hierarchy can activate this action to surface
    /// an error or informational message without needing a direct window ref.
    fn install_show_toast_action(&self) {
        let action = gio::SimpleAction::new("show-toast", Some(glib::VariantTy::STRING));
        let overlay_weak = self.imp().toast_overlay.downgrade();
        action.connect_activate(move |_, param| {
            let Some(overlay) = overlay_weak.upgrade() else {
                return;
            };
            let Some(msg) = param.and_then(|v| v.get::<String>()) else {
                return;
            };
            let toast = adw::Toast::new(&msg);
            toast.set_timeout(5);
            overlay.add_toast(toast);
        });
        self.add_action(&action);
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
            let Some(sv) = split_weak.upgrade() else {
                return;
            };
            if sv.is_collapsed() {
                let show_content = !sv.shows_content();
                sv.set_show_content(show_content);
                act.set_state(&(!show_content).to_variant()); // state = sidebar visible
            }
        });

        self.add_action(&action);
    }
}

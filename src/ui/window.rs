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

use crate::app_event::AppEvent;
use gtk::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use tracing::{debug, instrument};

use crate::library::album::AlbumId;

/// Wrapper for a reload callback that implements `Debug` and `Default`.
pub struct ReloadCallback(Box<dyn Fn()>);

impl ReloadCallback {
    pub fn new(f: impl Fn() + 'static) -> Self {
        Self(Box::new(f))
    }

    pub fn call(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for ReloadCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReloadCallback(..)")
    }
}

impl Default for ReloadCallback {
    fn default() -> Self {
        Self(Box::new(|| {}))
    }
}
use crate::library::Library;

use crate::ui::album_dialogs;
use crate::ui::collection_grid::CollectionGridView;
use crate::ui::coordinator::ContentCoordinator;
use crate::ui::empty_library::EmptyLibraryView;
use crate::ui::photo_grid::{PhotoGridModel, PhotoGridView};
use crate::ui::photo_grid::texture_cache::TextureCache;
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
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub split_view: TemplateChild<adw::NavigationSplitView>,

        /// Set up once in `setup()` — holds live references to all registered views.
        pub coordinator: OnceCell<Rc<RefCell<ContentCoordinator>>>,

        /// Sidebar reference for event-driven album updates.
        pub sidebar: OnceCell<MomentsSidebar>,

        /// GSettings instance for persisting window geometry.
        pub settings: OnceCell<gio::Settings>,

        /// Callback to reload the People collection grid after sync.
        #[allow(missing_debug_implementations)]
        pub people_reload: RefCell<Option<super::ReloadCallback>>,
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

    /// Wire the library model into the shell and switch to the content page.
    ///
    /// Builds the sidebar, registers all content views with the coordinator,
    /// then switches `main_stack` from "loading" to "content".
    /// Wire the library into the shell and switch to the content page.
    ///
    /// Photos is created eagerly (always the default view). Other routes
    /// are registered lazily — their views are materialised on first
    /// Wire the full shell: sidebar, coordinator, views.
    ///
    /// All models subscribe to the [`EventBus`] for event delivery.
    /// The caller does not need to forward events — components are
    /// self-contained.
    pub fn setup(
        &self,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        settings: gio::Settings,
        bus: &crate::event_bus::EventBus,
    ) {
        let imp = self.imp();
        use crate::library::media::MediaFilter;

        let bus_sender = bus.sender();

        // Build sidebar — MomentsSidebar is already an AdwNavigationPage subclass.
        let sidebar = MomentsSidebar::new();
        sidebar.subscribe_to_bus();
        imp.split_view.set_sidebar(Some(&sidebar));
        let _ = imp.sidebar.set(sidebar.clone());

        // Populate sidebar with existing albums from the library.
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let sb = sidebar.clone();
            let tx = bus_sender.clone();
            glib::MainContext::default().spawn_local(async move {
                match tk.spawn(async move { lib.list_albums().await }).await {
                    Ok(Ok(albums)) => {
                        let pairs: Vec<(String, String)> = albums
                            .into_iter()
                            .map(|a| (a.id.as_str().to_owned(), a.name))
                            .collect();
                        sb.set_albums(&pairs);
                    }
                    Ok(Err(e)) => {
                        tracing::error!("failed to load albums for sidebar: {e}");
                        tx.send(AppEvent::Error("Could not load albums".into()));
                    }
                    Err(e) => {
                        tracing::error!("failed to load albums for sidebar (join): {e}");
                        tx.send(AppEvent::Error("Could not load albums".into()));
                    }
                }
            });
        }

        // Wire "+" button → create album dialog.
        {
            let win_weak = self.downgrade();
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let sb = sidebar.clone();
            sidebar.connect_album_add_clicked(move || {
                let Some(win) = win_weak.upgrade() else { return };
                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                let sb = sb.clone();
                album_dialogs::show_create_album_dialog(&win, move |name| {
                    let lib = Arc::clone(&lib);
                    let tk = tk.clone();
                    let sb = sb.clone();
                    glib::MainContext::default().spawn_local(async move {
                        let n = name.clone();
                        match tk.spawn(async move { lib.create_album(&n).await }).await {
                            Ok(Ok(id)) => {
                                debug!(album_id = %id, name = %name, "album created");
                                sb.add_album(id.as_str(), &name);
                            }
                            Ok(Err(e)) => {
                                tracing::error!("failed to create album: {e}");
                                let _ = sb.activate_action("win.show-toast", Some(&"Failed to create album".to_variant()));
                            }
                            Err(e) => tracing::error!("tokio join error: {e}"),
                        }
                    });
                });
            });
        }

        // Wire right-click context menu on album rows.
        {
            let win_weak = self.downgrade();
            let lib_rename = Arc::clone(&library);
            let tk_rename = tokio.clone();
            let sb_rename = sidebar.clone();

            let win_weak2 = self.downgrade();
            let lib_delete = Arc::clone(&library);
            let tk_delete = tokio.clone();
            let sb_delete = sidebar.clone();

            sidebar.set_album_context_callbacks(
                // on_rename callback
                move |album_id, album_name| {
                    let Some(win) = win_weak.upgrade() else { return };
                    let lib = Arc::clone(&lib_rename);
                    let tk = tk_rename.clone();
                    let sb = sb_rename.clone();
                    let aid = album_id.clone();
                    album_dialogs::show_rename_album_dialog(&win, &album_name, move |new_name| {
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let sb = sb.clone();
                        let aid = aid.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let n = new_name.clone();
                            let id = AlbumId::from_raw(aid.clone());
                            match tk.spawn(async move { lib.rename_album(&id, &n).await }).await {
                                Ok(Ok(())) => {
                                    debug!(album_id = %aid, name = %new_name, "album renamed");
                                    sb.rename_album(&aid, &new_name);
                                }
                                Ok(Err(e)) => {
                                    tracing::error!("failed to rename album: {e}");
                                    let _ = sb.activate_action("win.show-toast", Some(&"Failed to rename album".to_variant()));
                                }
                                Err(e) => tracing::error!("tokio join error: {e}"),
                            }
                        });
                    });
                },
                // on_delete callback
                move |album_id, album_name| {
                    let Some(win) = win_weak2.upgrade() else { return };
                    let lib = Arc::clone(&lib_delete);
                    let tk = tk_delete.clone();
                    let sb = sb_delete.clone();
                    let aid = album_id.clone();
                    let win_weak_inner = win.downgrade();
                    album_dialogs::show_delete_album_dialog(&win, &album_name, move || {
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let sb = sb.clone();
                        let aid = aid.clone();
                        let win_w = win_weak_inner.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let id = AlbumId::from_raw(aid.clone());
                            match tk.spawn(async move { lib.delete_album(&id).await }).await {
                                Ok(Ok(())) => {
                                    debug!(album_id = %aid, "album deleted");
                                    sb.remove_album(&aid);
                                    if let Some(win) = win_w.upgrade() {
                                        let route = format!("album:{aid}");
                                        if let Some(coord) = win.imp().coordinator.get() {
                                            coord.borrow_mut().unregister(&route);
                                        }
                                    }
                                }
                                Ok(Err(e)) => {
                                    tracing::error!("failed to delete album: {e}");
                                    let _ = sb.activate_action("win.show-toast", Some(&"Failed to delete album".to_variant()));
                                }
                                Err(e) => tracing::error!("tokio join error: {e}"),
                            }
                        });
                    });
                },
            );
        }

        // Build content stack + coordinator.
        let content_stack = gtk::Stack::new();
        content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        let mut coordinator = ContentCoordinator::new(content_stack.clone());

        // Shared LRU cache for decoded thumbnail pixels — avoids re-decoding
        // when scrolling back through previously-visible cells.
        let texture_cache = Rc::new(TextureCache::new());

        // Register the empty-library view (eager, no model).
        coordinator.register("empty", Rc::new(EmptyLibraryView::new()));

        // Register the Photos view (eager — always the default).
        let photos_model = Rc::new(PhotoGridModel::new(
            Arc::clone(&library),
            tokio.clone(),
            MediaFilter::All,
            bus_sender.clone(),
        ));
        let photos_view = Rc::new(PhotoGridView::new(
            Arc::clone(&library),
            tokio.clone(),
            settings.clone(),
            Rc::clone(&texture_cache),
            bus_sender.clone(),
        ));
        photos_view.set_model(Rc::clone(&photos_model));
        photos_model.subscribe(bus);
        coordinator.register("photos", photos_view);

        // Register the Favorites view (lazy — created on first click).
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings.clone();
            let tc = Rc::clone(&texture_cache);
            let bs = bus_sender.clone();
            coordinator.register_lazy("favorites", move || {
                let model = Rc::new(PhotoGridModel::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    MediaFilter::Favorites,
                    bs.clone(),
                ));
                let view = Rc::new(PhotoGridView::new(lib, tk, s, tc, bs));
                view.set_model(Rc::clone(&model));
                model.subscribe_to_bus();
                view
            });
        }

        // Register the Recent Imports view (lazy — created on first click).
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings.clone();
            let tc = Rc::clone(&texture_cache);
            let bs = bus_sender.clone();
            coordinator.register_lazy("recent", move || {
                let days = s.uint("recent-imports-days") as i64;
                let since = chrono::Utc::now().timestamp() - days * 86400;
                let model = Rc::new(PhotoGridModel::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    MediaFilter::RecentImports { since },
                    bs.clone(),
                ));
                let view = Rc::new(PhotoGridView::new(lib, tk, s, tc, bs));
                view.set_model(Rc::clone(&model));
                model.subscribe_to_bus();
                view
            });
        }

        // Register the Trash view (lazy — created on first click).
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings.clone();
            let tc = Rc::clone(&texture_cache);
            let bs = bus_sender.clone();
            coordinator.register_lazy("trash", move || {
                let model = Rc::new(PhotoGridModel::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    MediaFilter::Trashed,
                    bs.clone(),
                ));
                let view = Rc::new(PhotoGridView::new(lib, tk, s, tc, bs));
                view.set_model(Rc::clone(&model));
                model.subscribe_to_bus();
                view
            });
        }

        // Register the People collection view (lazy — created on first click).
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings.clone();
            let tc = Rc::clone(&texture_cache);
            let bs = bus_sender.clone();
            let win_weak = self.downgrade();
            coordinator.register_lazy("people", move || {
                let view = Rc::new(CollectionGridView::new_people(lib, tk, s, tc, bs));
                // Store reload callback so PeopleSyncComplete can refresh the grid.
                if let Some(win) = win_weak.upgrade() {
                    let view_ref = Rc::clone(&view);
                    *win.imp().people_reload.borrow_mut() = Some(ReloadCallback::new(move || {
                        view_ref.reload();
                    }));
                }
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
        // Album routes are registered dynamically on first click.
        {
            let obj_weak = self.downgrade();
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings.clone();
            let tc = Rc::clone(&texture_cache);
            let bs = bus_sender.clone();
            sidebar.connect_route_selected(move |id| {
                debug!(route = %id, "sidebar route selected");
                let Some(win) = obj_weak.upgrade() else { return };
                let Some(coordinator) = win.imp().coordinator.get() else { return };

                // Dynamic registration for album routes.
                if let Some(album_id_str) = id.strip_prefix("album:") {
                    let mut coord = coordinator.borrow_mut();
                    if !coord.has_route(id) {
                        let album_id = AlbumId::from_raw(album_id_str.to_owned());
                        let model = Rc::new(PhotoGridModel::new(
                            Arc::clone(&lib),
                            tk.clone(),
                            MediaFilter::Album { album_id },
                            bs.clone(),
                        ));
                        let view = Rc::new(PhotoGridView::new(
                            Arc::clone(&lib),
                            tk.clone(),
                            s.clone(),
                            Rc::clone(&tc),
                            bs.clone(),
                        ));
                        view.set_model(Rc::clone(&model));
                        model.subscribe_to_bus();
                        coord.register(id, view);
                        debug!(route = %id, "registered album view");
                    }
                    if let Some(actions) = coord.navigate(id) {
                        win.insert_action_group("view", Some(&actions));
                    }
                } else {
                    if let Some(actions) = coordinator.borrow_mut().navigate(id) {
                        win.insert_action_group("view", Some(&actions));
                    }
                }
            });
        }

        sidebar.select_first();

        // Add window-level actions.
        self.install_show_toast_action();
        self.install_toggle_sidebar_action();

        debug!("switching main window to content page");
        imp.main_stack.set_visible_child_name("content");
    }

    /// Access the sidebar for event-driven album updates.
    pub fn sidebar(&self) -> Option<&MomentsSidebar> {
        self.imp().sidebar.get()
    }

    /// Navigate to the given route by id (e.g. "recent", "photos").
    pub fn navigate(&self, route_id: &str) {
        if let Some(coordinator) = self.imp().coordinator.get() {
            if let Some(actions) = coordinator.borrow_mut().navigate(route_id) {
                self.insert_action_group("view", Some(&actions));
            }
        }
    }

    /// Reload the People collection grid if it has been materialised.
    pub fn reload_people(&self) {
        if let Some(reload) = self.imp().people_reload.borrow().as_ref() {
            reload.call();
        }
    }

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
            let Some(overlay) = overlay_weak.upgrade() else { return };
            let Some(msg) = param.and_then(|v| v.get::<String>()) else { return };
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

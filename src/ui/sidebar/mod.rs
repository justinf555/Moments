pub mod route;
mod status_bar;

use std::cell::Cell;
use std::cell::RefCell;

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{gio, glib, subclass::prelude::*};
use tracing::debug;

pub use status_bar::StatusBar;

use route::ROUTES;

mod imp {
    use super::*;
    use std::cell::OnceCell;
    use std::rc::Rc;

    pub struct MomentsSidebar {
        pub sidebar: OnceCell<adw::Sidebar>,
        pub route_section: OnceCell<adw::SidebarSection>,
        pub people_item: OnceCell<adw::SidebarItem>,
        /// Route IDs in display order (matches sidebar item indices).
        pub active_routes: RefCell<Vec<&'static str>>,
        pub pinned_section: OnceCell<adw::SidebarSection>,
        /// Filtered model of pinned albums — drives the pinned sidebar section.
        pub pinned_model: OnceCell<gtk::FilterListModel>,
        pub trash_badge: OnceCell<gtk::Label>,
        pub trash_count: Cell<u32>,

        /// Status bar controller (sync/upload/idle states).
        pub status_bar: OnceCell<super::StatusBar>,

        /// Keeps the event bus subscription alive for this sidebar's lifetime.
        pub _subscription: RefCell<Option<crate::event_bus::Subscription>>,
        /// Signal handler IDs for ImportClient property notifications.
        pub _import_handlers: RefCell<Vec<glib::SignalHandlerId>>,
    }

    impl Default for MomentsSidebar {
        fn default() -> Self {
            Self {
                sidebar: OnceCell::new(),
                route_section: OnceCell::new(),
                people_item: OnceCell::new(),
                active_routes: RefCell::new(Vec::new()),
                pinned_section: OnceCell::new(),
                pinned_model: OnceCell::new(),
                trash_badge: OnceCell::new(),
                trash_count: Cell::new(0),
                status_bar: OnceCell::new(),
                _subscription: RefCell::new(None),
                _import_handlers: RefCell::new(Vec::new()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsSidebar {
        const NAME: &'static str = "MomentsSidebar";
        type Type = super::MomentsSidebar;
        type ParentType = adw::NavigationPage;
    }

    impl MomentsSidebar {
        pub fn sidebar(&self) -> &adw::Sidebar {
            self.sidebar.get().expect("sidebar not initialized")
        }
        pub fn pinned_section(&self) -> &adw::SidebarSection {
            self.pinned_section
                .get()
                .expect("pinned_section not initialized")
        }
        pub fn status_bar(&self) -> &super::StatusBar {
            self.status_bar
                .get()
                .expect("status_bar not initialized")
        }
    }

    impl ObjectImpl for MomentsSidebar {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.set_title("Moments");

            let toolbar_view = adw::ToolbarView::new();
            toolbar_view.add_top_bar(&build_header_bar());

            let sidebar = adw::Sidebar::new();
            sidebar.append(self.build_route_section());
            self.build_pinned_section(&obj, &sidebar, &toolbar_view);
            toolbar_view.set_content(Some(&sidebar));

            let status_bar = super::StatusBar::new(&toolbar_view);
            obj.set_child(Some(status_bar.bottom_sheet()));

            self.sidebar.set(sidebar).expect("set once in constructed");
            self.status_bar
                .set(status_bar)
                .expect("set once in constructed");
        }
    }

    impl MomentsSidebar {
        fn build_route_section(&self) -> adw::SidebarSection {
            let section = adw::SidebarSection::new();
            let mut active = Vec::new();

            for route in ROUTES.iter() {
                let mut builder = adw::SidebarItem::builder()
                    .title(gettext(route.label))
                    .icon_name(route.icon);

                if route.id == "trash" {
                    let badge = gtk::Label::new(None);
                    badge.add_css_class("sidebar-badge");
                    badge.set_visible(false);
                    builder = builder.suffix(&badge);
                    let _ = self.trash_badge.set(badge);
                }

                let item = builder.build();

                if route.id == "people" {
                    let _ = self.people_item.set(item.clone());
                }

                section.append(item);
                active.push(route.id);
            }

            *self.active_routes.borrow_mut() = active;
            let _ = self.route_section.set(section.clone());

            section
        }

        fn build_pinned_section(
            &self,
            obj: &<Self as ObjectSubclass>::Type,
            sidebar: &adw::Sidebar,
            toolbar_view: &adw::ToolbarView,
        ) {
            let pinned_section = adw::SidebarSection::new();
            pinned_section.set_title(Some(&gettext("Pinned")));

            let unpin_menu = gio::Menu::new();
            unpin_menu.append(Some(&gettext("Unpin from Sidebar")), Some("sidebar.unpin"));
            pinned_section.set_menu_model(Some(&unpin_menu));

            sidebar.append(pinned_section.clone());
            let _ = self.pinned_section.set(pinned_section.clone());

            // Build the filtered model — populated later in setup_pinned_albums.
            let filter = gtk::CustomFilter::new(|obj| {
                obj.downcast_ref::<crate::client::AlbumItemObject>()
                    .is_some_and(|item| item.pinned())
            });
            let pinned_model = gtk::FilterListModel::new(
                None::<gio::ListModel>,
                Some(filter),
            );
            let _ = self.pinned_model.set(pinned_model);

            // Unpin action — resolves album from pinned model index.
            let menu_target_index: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
            {
                let mti = Rc::clone(&menu_target_index);
                let ps = pinned_section.clone();
                sidebar.connect_setup_menu(move |_, item| {
                    if let Some(item) = item {
                        let n = ps.items().n_items();
                        for i in 0..n {
                            if let Some(pinned_item) = ps.item(i) {
                                if pinned_item == *item {
                                    mti.set(Some(i));
                                    return;
                                }
                            }
                        }
                    }
                    mti.set(None);
                });
            }

            let unpin_action = gio::SimpleAction::new("unpin", None);
            {
                let mti = Rc::clone(&menu_target_index);
                let obj_weak = obj.downgrade();
                unpin_action.connect_activate(move |_, _| {
                    let Some(index) = mti.get() else { return };
                    let Some(sidebar) = obj_weak.upgrade() else {
                        return;
                    };
                    let Some(pinned_model) = sidebar.imp().pinned_model.get() else {
                        return;
                    };
                    if let Some(obj) = pinned_model
                        .item(index)
                        .and_then(|o| o.downcast::<crate::client::AlbumItemObject>().ok())
                    {
                        let album_client = crate::application::MomentsApplication::default()
                            .album_client_v2()
                            .expect("album client v2 available");
                        album_client.unpin_album(crate::library::album::AlbumId::from_raw(obj.id()));
                    }
                });
            }

            let sidebar_action_group = gio::SimpleActionGroup::new();
            sidebar_action_group.add_action(&unpin_action);
            toolbar_view.insert_action_group("sidebar", Some(&sidebar_action_group));
        }
    }

    fn build_header_bar() -> adw::HeaderBar {
        let header = adw::HeaderBar::new();

        let menu_button = gtk::MenuButton::builder()
            .primary(true)
            .icon_name("open-menu-symbolic")
            .tooltip_text(gettext("Main Menu"))
            .build();
        let menu = gio::Menu::new();
        let import_section = gio::Menu::new();
        import_section.append(Some("_Import"), Some("app.import"));
        menu.append_section(None, &import_section);
        let app_section = gio::Menu::new();
        app_section.append(Some("_Keyboard Shortcuts"), Some("app.shortcuts"));
        app_section.append(Some("_About Moments"), Some("app.about"));
        app_section.append(Some("_Preferences"), Some("app.preferences"));
        menu.append_section(None, &app_section);
        menu_button.set_menu_model(Some(&menu));
        header.pack_end(&menu_button);

        header
    }

    impl WidgetImpl for MomentsSidebar {
        fn realize(&self) {
            self.parent_realize();

            let weak = self.obj().downgrade();
            let sub = crate::event_bus::subscribe(move |event| {
                let Some(sidebar) = weak.upgrade() else {
                    return;
                };
                match event {
                    crate::app_event::AppEvent::SyncStarted => {
                        sidebar.imp().status_bar().show_sync_started();
                    }
                    crate::app_event::AppEvent::SyncProgress {
                        assets,
                        people,
                        faces,
                    } => {
                        sidebar.imp().status_bar().show_sync_progress(*assets, *people, *faces);
                    }
                    crate::app_event::AppEvent::SyncComplete { .. } => {
                        sidebar.imp().status_bar().show_sync_complete();
                    }
                    crate::app_event::AppEvent::Trashed { ids } => {
                        sidebar.adjust_trash_count(ids.len() as i32);
                    }
                    crate::app_event::AppEvent::Restored { ids } => {
                        sidebar.adjust_trash_count(-(ids.len() as i32));
                    }
                    crate::app_event::AppEvent::Deleted { ids } => {
                        sidebar.adjust_trash_count(-(ids.len() as i32));
                    }
                    _ => {}
                }
            });
            *self._subscription.borrow_mut() = Some(sub);

            // Connect to ImportClient property notifications.
            if let Some(import_client) =
                crate::application::MomentsApplication::default().import_client()
            {
                let mut handlers = self._import_handlers.borrow_mut();
                let obj = self.obj().clone();

                // Progress updates.
                let weak = obj.downgrade();
                handlers.push(import_client.connect_notify_local(
                    Some("current"),
                    move |client, _| {
                        if let Some(sidebar) = weak.upgrade() {
                            sidebar.imp().status_bar().show_upload_progress(
                                client.current() as usize,
                                client.total() as usize,
                                client.imported() as usize,
                                client.skipped() as usize,
                                client.failed() as usize,
                            );
                        }
                    },
                ));

                // State changes (completion).
                let weak = obj.downgrade();
                handlers.push(import_client.connect_notify_local(
                    Some("state"),
                    move |client, _| {
                        if let Some(sidebar) = weak.upgrade() {
                            if client.state() == crate::client::import_client::ImportState::Complete
                            {
                                let summary = crate::importer::ImportSummary {
                                    imported: client.imported() as usize,
                                    skipped_duplicates: client.skipped() as usize,
                                    skipped_unsupported: 0,
                                    failed: client.failed() as usize,
                                    elapsed_secs: client.elapsed_secs(),
                                };
                                sidebar.imp().status_bar().show_upload_complete(&summary);
                            }
                        }
                    },
                ));
            }
        }

        fn unrealize(&self) {
            self._subscription.borrow_mut().take();
            // Disconnect ImportClient signal handlers.
            if let Some(import_client) =
                crate::application::MomentsApplication::default().import_client()
            {
                for handler_id in self._import_handlers.borrow_mut().drain(..) {
                    import_client.disconnect(handler_id);
                }
            }
            self.parent_unrealize();
        }
    }
    impl adw::subclass::prelude::NavigationPageImpl for MomentsSidebar {}
}

glib::wrapper! {
    pub struct MomentsSidebar(ObjectSubclass<imp::MomentsSidebar>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for MomentsSidebar {
    fn default() -> Self {
        Self::new()
    }
}

impl MomentsSidebar {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Connect a callback that fires when the user activates a sidebar item.
    ///
    /// System routes (index 0–5) map to `ROUTES[index].id`.
    /// Pinned album items (index 6+) map to `"album:{album_id}"`.
    pub fn connect_route_selected<F: Fn(&str) + 'static>(&self, f: F) {
        let sidebar = self.imp().sidebar().clone();
        let weak = self.downgrade();
        sidebar.connect_activated(move |_, index| {
            let Some(sb) = weak.upgrade() else { return };
            let active = sb.imp().active_routes.borrow();
            let system_count = active.len() as u32;
            if index < system_count {
                if let Some(route_id) = active.get(index as usize) {
                    debug!(route = %route_id, "sidebar route selected");
                    f(route_id);
                }
            } else {
                // Pinned album item — resolve from filtered model.
                let pinned_index = (index - system_count) as usize;
                if let Some(pinned_model) = sb.imp().pinned_model.get() {
                    if let Some(obj) = pinned_model
                        .item(pinned_index as u32)
                        .and_then(|o| o.downcast::<crate::client::AlbumItemObject>().ok())
                    {
                        let route = format!("album:{}", obj.id());
                        debug!(route = %route, "pinned album selected");
                        f(&route);
                    }
                }
            }
        });
    }

    /// Hide the People sidebar item (used for Local backend which has no face detection).
    pub fn hide_people(&self) {
        let imp = self.imp();
        if let (Some(section), Some(item)) = (imp.route_section.get(), imp.people_item.get()) {
            section.remove(item);
            imp.active_routes.borrow_mut().retain(|id| *id != "people");
            debug!("people route hidden (local backend)");
        }
    }

    /// Pre-select the first item (Photos) so the shell always has an active route.
    pub fn select_first(&self) {
        self.imp().sidebar().set_selected(0);
    }

    /// Set the initial trash count (called once at startup after querying the library).
    pub fn set_trash_count(&self, count: u32) {
        let imp = self.imp();
        imp.trash_count.set(count);
        self.update_trash_badge();
    }

    /// Adjust the trash count by a signed delta.
    fn adjust_trash_count(&self, delta: i32) {
        let imp = self.imp();
        let current = imp.trash_count.get() as i32;
        let new_count = (current + delta).max(0) as u32;
        imp.trash_count.set(new_count);
        self.update_trash_badge();
    }

    // ── Pinned albums ───────────────────────────────────────────────

    /// Wire the pinned album section to the album client's model.
    ///
    /// Creates a filtered view of pinned albums and reactively syncs
    /// sidebar items when albums are pinned or unpinned.
    pub fn setup_pinned_albums(&self) {
        let imp = self.imp();

        let album_client = crate::application::MomentsApplication::default()
            .album_client_v2()
            .expect("album client v2 available");

        let store = album_client.create_model();
        album_client.list_albums(&store);

        let Some(pinned_model) = imp.pinned_model.get() else {
            return;
        };
        pinned_model.set_model(Some(&store));

        // Build initial items from any already-loaded pinned albums.
        self.rebuild_pinned_items();

        // React to filter model changes — rebuild sidebar items.
        let weak = self.downgrade();
        pinned_model.connect_items_changed(move |_, _, _, _| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.rebuild_pinned_items();
            }
        });
    }

    /// Rebuild the pinned sidebar section from the filtered model.
    fn rebuild_pinned_items(&self) {
        let imp = self.imp();
        let section = imp.pinned_section();
        let Some(pinned_model) = imp.pinned_model.get() else {
            return;
        };

        section.remove_all();

        for i in 0..pinned_model.n_items() {
            if let Some(obj) = pinned_model
                .item(i)
                .and_then(|o| o.downcast::<crate::client::AlbumItemObject>().ok())
            {
                let item = adw::SidebarItem::builder()
                    .title(&obj.name())
                    .icon_name("folder-symbolic")
                    .build();
                section.append(item);
            }
        }

        debug!(count = pinned_model.n_items(), "pinned albums rebuilt");
    }

    /// Update the Trash badge with the current count.
    fn update_trash_badge(&self) {
        let imp = self.imp();
        if let Some(badge) = imp.trash_badge.get() {
            let count = imp.trash_count.get();
            if count > 0 {
                let label =
                    ngettext("{} item", "{} items", count).replace("{}", &count.to_string());
                badge.set_label(&label);
                badge.set_visible(true);
            } else {
                badge.set_visible(false);
            }
        }
    }

}

pub mod route;

use std::cell::Cell;
use std::cell::RefCell;

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{gio, glib, subclass::prelude::*};
use tracing::debug;

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

        // ── Bottom sheet (upload detail) ──────────────────────────────────
        pub bottom_sheet: OnceCell<adw::BottomSheet>,
        pub progress_label: OnceCell<gtk::Label>,
        pub progress_bar: OnceCell<gtk::ProgressBar>,
        pub detail_label: OnceCell<gtk::Label>,

        // ── Status bar stack ──────────────────────────────────────────────
        pub bar_stack: OnceCell<gtk::Stack>,
        pub idle_label: OnceCell<gtk::Label>,
        pub sync_label: OnceCell<gtk::Label>,
        pub thumb_label: OnceCell<gtk::Label>,
        pub upload_label: OnceCell<gtk::Label>,
        pub complete_label: OnceCell<gtk::Label>,

        /// Unix timestamp of last successful sync completion.
        pub last_synced_at: Cell<Option<i64>>,
        /// Timer ID for updating the "Synced X ago" label.
        pub sync_timer: RefCell<Option<glib::SourceId>>,
        /// Current status bar state (for priority logic).
        pub current_state: Cell<StatusState>,
        /// Keeps the event bus subscription alive for this sidebar's lifetime.
        pub _subscription: RefCell<Option<crate::event_bus::Subscription>>,
        /// Signal handler IDs for ImportClient property notifications.
        pub _import_handlers: RefCell<Vec<glib::SignalHandlerId>>,
    }

    /// Tracks the active bottom bar state for priority-based switching.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub enum StatusState {
        Idle = 0,
        Thumbnails = 1,
        Sync = 2,
        Complete = 3,
        Upload = 4,
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
                bottom_sheet: OnceCell::new(),
                progress_label: OnceCell::new(),
                progress_bar: OnceCell::new(),
                detail_label: OnceCell::new(),
                bar_stack: OnceCell::new(),
                idle_label: OnceCell::new(),
                sync_label: OnceCell::new(),
                thumb_label: OnceCell::new(),
                upload_label: OnceCell::new(),
                complete_label: OnceCell::new(),
                last_synced_at: Cell::new(None),
                sync_timer: RefCell::new(None),
                current_state: Cell::new(StatusState::Idle),
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
        pub fn bar_stack(&self) -> &gtk::Stack {
            self.bar_stack.get().expect("bar_stack not initialized")
        }
        pub fn bottom_sheet(&self) -> &adw::BottomSheet {
            self.bottom_sheet
                .get()
                .expect("bottom_sheet not initialized")
        }
        pub fn idle_label(&self) -> &gtk::Label {
            self.idle_label.get().expect("idle_label not initialized")
        }
        pub fn sync_label(&self) -> &gtk::Label {
            self.sync_label.get().expect("sync_label not initialized")
        }
        pub fn thumb_label(&self) -> &gtk::Label {
            self.thumb_label.get().expect("thumb_label not initialized")
        }
        pub fn upload_label(&self) -> &gtk::Label {
            self.upload_label
                .get()
                .expect("upload_label not initialized")
        }
        pub fn complete_label(&self) -> &gtk::Label {
            self.complete_label
                .get()
                .expect("complete_label not initialized")
        }
        pub fn progress_label(&self) -> &gtk::Label {
            self.progress_label
                .get()
                .expect("progress_label not initialized")
        }
        pub fn progress_bar(&self) -> &gtk::ProgressBar {
            self.progress_bar
                .get()
                .expect("progress_bar not initialized")
        }
        pub fn detail_label(&self) -> &gtk::Label {
            self.detail_label
                .get()
                .expect("detail_label not initialized")
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

            let StatusBarWidgets {
                bar_stack,
                idle_label,
                sync_label,
                thumb_label,
                upload_label,
                complete_label,
            } = build_status_bar_stack();
            let UploadDetailWidgets {
                sheet_box,
                progress_label,
                progress_bar,
                detail_label,
            } = build_upload_detail_sheet();

            let bottom_sheet = build_bottom_sheet(&toolbar_view, &sheet_box, &bar_stack);
            obj.set_child(Some(&bottom_sheet));

            self.sidebar.set(sidebar).expect("set once in constructed");
            self.bottom_sheet
                .set(bottom_sheet)
                .expect("set once in constructed");
            self.progress_label
                .set(progress_label)
                .expect("set once in constructed");
            self.progress_bar
                .set(progress_bar)
                .expect("set once in constructed");
            self.detail_label
                .set(detail_label)
                .expect("set once in constructed");
            self.bar_stack
                .set(bar_stack)
                .expect("set once in constructed");
            self.idle_label
                .set(idle_label)
                .expect("set once in constructed");
            self.sync_label
                .set(sync_label)
                .expect("set once in constructed");
            self.thumb_label
                .set(thumb_label)
                .expect("set once in constructed");
            self.upload_label
                .set(upload_label)
                .expect("set once in constructed");
            self.complete_label
                .set(complete_label)
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

    struct StatusBarWidgets {
        bar_stack: gtk::Stack,
        idle_label: gtk::Label,
        sync_label: gtk::Label,
        thumb_label: gtk::Label,
        upload_label: gtk::Label,
        complete_label: gtk::Label,
    }

    fn build_status_bar_page(
        icon_name: &str,
        text: &str,
        extra_icon_classes: &[&str],
        extra_label_classes: &[&str],
        margins: (i32, i32),
    ) -> (gtk::Box, gtk::Label) {
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hbox.set_margin_start(12);
        hbox.set_margin_end(12);
        hbox.set_margin_top(margins.0);
        hbox.set_margin_bottom(margins.1);
        let icon = gtk::Image::from_icon_name(icon_name);
        for cls in extra_icon_classes {
            icon.add_css_class(cls);
        }
        hbox.append(&icon);
        let label = gtk::Label::new(Some(text));
        label.set_hexpand(true);
        label.set_xalign(0.0);
        label.add_css_class("caption");
        for cls in extra_label_classes {
            label.add_css_class(cls);
        }
        hbox.append(&label);
        (hbox, label)
    }

    fn build_status_bar_stack() -> StatusBarWidgets {
        let bar_stack = gtk::Stack::new();
        bar_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        bar_stack.set_transition_duration(200);

        let (idle_box, idle_label) = build_status_bar_page(
            "object-select-symbolic",
            "Waiting for sync...",
            &["dim-label"],
            &["dim-label"],
            (8, 8),
        );
        bar_stack.add_named(&idle_box, Some("idle"));

        let (sync_box, sync_label) =
            build_status_bar_page("view-refresh-symbolic", "Syncing...", &[], &[], (8, 8));
        bar_stack.add_named(&sync_box, Some("sync"));

        let (thumb_box, thumb_label) = build_status_bar_page(
            "folder-download-symbolic",
            "Downloading thumbnails...",
            &[],
            &[],
            (8, 8),
        );
        bar_stack.add_named(&thumb_box, Some("thumbnails"));

        let (upload_box, upload_label) =
            build_status_bar_page("go-up-symbolic", "Uploading...", &[], &[], (12, 16));
        bar_stack.add_named(&upload_box, Some("upload"));

        let (complete_box, complete_label) = build_status_bar_page(
            "object-select-symbolic",
            "Import complete",
            &[],
            &[],
            (8, 8),
        );
        bar_stack.add_named(&complete_box, Some("complete"));

        StatusBarWidgets {
            bar_stack,
            idle_label,
            sync_label,
            thumb_label,
            upload_label,
            complete_label,
        }
    }

    struct UploadDetailWidgets {
        sheet_box: gtk::Box,
        progress_label: gtk::Label,
        progress_bar: gtk::ProgressBar,
        detail_label: gtk::Label,
    }

    fn build_upload_detail_sheet() -> UploadDetailWidgets {
        let sheet_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        sheet_box.set_margin_start(16);
        sheet_box.set_margin_end(16);
        sheet_box.set_margin_top(16);
        sheet_box.set_margin_bottom(16);

        let progress_label = gtk::Label::new(Some("Uploading..."));
        progress_label.set_xalign(0.0);
        progress_label.add_css_class("heading");
        sheet_box.append(&progress_label);

        let progress_bar = gtk::ProgressBar::new();
        progress_bar.set_fraction(0.0);
        sheet_box.append(&progress_bar);

        let detail_label = gtk::Label::new(Some(""));
        detail_label.set_xalign(0.0);
        detail_label.add_css_class("dim-label");
        detail_label.add_css_class("caption");
        sheet_box.append(&detail_label);

        UploadDetailWidgets {
            sheet_box,
            progress_label,
            progress_bar,
            detail_label,
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

    fn build_bottom_sheet(
        toolbar_view: &adw::ToolbarView,
        sheet_box: &gtk::Box,
        bar_stack: &gtk::Stack,
    ) -> adw::BottomSheet {
        let bottom_sheet = adw::BottomSheet::new();
        bottom_sheet.set_content(Some(toolbar_view));
        bottom_sheet.set_sheet(Some(sheet_box));
        bottom_sheet.set_bottom_bar(Some(bar_stack));
        bottom_sheet.set_open(false);
        bottom_sheet.set_show_drag_handle(false);
        bottom_sheet.set_can_open(false);
        bottom_sheet.set_modal(false);
        bottom_sheet.set_full_width(true);
        bottom_sheet.set_reveal_bottom_bar(true);
        bottom_sheet
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
                        sidebar.show_sync_started();
                    }
                    crate::app_event::AppEvent::SyncProgress {
                        assets,
                        people,
                        faces,
                    } => {
                        sidebar.show_sync_progress(*assets, *people, *faces);
                    }
                    crate::app_event::AppEvent::SyncComplete { assets, .. } => {
                        sidebar.show_sync_complete(*assets);
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
                            sidebar.show_upload_progress(
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
                                sidebar.show_upload_complete(&summary);
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

    // ── Status bar methods ───────────────────────────────────────────

    fn set_status(&self, state: imp::StatusState, page: &str) {
        let imp = self.imp();
        let current = imp.current_state.get();

        if state >= current || state == imp::StatusState::Idle {
            imp.current_state.set(state);
            imp.bar_stack().set_visible_child_name(page);
            if state != imp::StatusState::Upload {
                let sheet = imp.bottom_sheet();
                sheet.set_can_open(false);
                sheet.set_open(false);
            }
        }
    }

    pub fn set_idle(&self) {
        self.set_status(imp::StatusState::Idle, "idle");
        self.update_idle_label();
        self.start_idle_timer();
    }

    pub fn show_sync_started(&self) {
        let imp = self.imp();
        imp.sync_label().set_text("Syncing...");
        self.set_status(imp::StatusState::Sync, "sync");
    }

    pub fn show_sync_progress(&self, assets: usize, people: usize, faces: usize) {
        let imp = self.imp();
        let total = assets + people + faces;
        imp.sync_label()
            .set_text(&format!("Syncing... {total} items"));
        self.set_status(imp::StatusState::Sync, "sync");
    }

    pub fn show_sync_complete(&self, _assets: usize) {
        let imp = self.imp();
        imp.last_synced_at.set(Some(chrono::Utc::now().timestamp()));

        let current = imp.current_state.get();
        if current == imp::StatusState::Idle || current == imp::StatusState::Sync {
            self.set_idle();
        } else {
            let obj_weak = self.downgrade();
            glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
                if let Some(obj) = obj_weak.upgrade() {
                    let state = obj.imp().current_state.get();
                    if state == imp::StatusState::Thumbnails {
                        obj.set_idle();
                    }
                }
            });
        }
    }

    pub fn show_upload_progress(
        &self,
        current: usize,
        total: usize,
        imported: usize,
        skipped: usize,
        failed: usize,
    ) {
        let imp = self.imp();
        imp.upload_label()
            .set_text(&format!("Uploading {current}/{total}"));
        imp.progress_label()
            .set_text(&format!("Uploading {current} of {total}"));
        if total > 0 {
            imp.progress_bar()
                .set_fraction(current as f64 / total as f64);
        }
        let mut detail = format!("{imported} imported");
        if skipped > 0 {
            detail.push_str(&format!(", {skipped} skipped"));
        }
        if failed > 0 {
            detail.push_str(&format!(", {failed} failed"));
        }
        imp.detail_label().set_text(&detail);
        let sheet = imp.bottom_sheet();
        if !sheet.is_open() {
            sheet.set_can_open(true);
            sheet.set_open(true);
        }
        self.set_status(imp::StatusState::Upload, "upload");
    }

    pub fn show_upload_complete(&self, summary: &crate::importer::ImportSummary) {
        let imp = self.imp();

        let mut bar_text = format!("{} imported", summary.imported);
        if summary.skipped_duplicates > 0 {
            bar_text.push_str(&format!(", {} skipped", summary.skipped_duplicates));
        }
        if summary.failed > 0 {
            bar_text.push_str(&format!(", {} failed", summary.failed));
        }

        imp.complete_label().set_text("Upload Complete");
        imp.progress_label().set_text(&bar_text);
        imp.progress_bar().set_fraction(1.0);
        imp.detail_label().set_text(&bar_text);

        imp.bottom_sheet().set_open(false);

        self.set_status(imp::StatusState::Complete, "complete");

        let obj_weak = self.downgrade();
        glib::timeout_add_local_once(std::time::Duration::from_secs(5), move || {
            if let Some(obj) = obj_weak.upgrade() {
                obj.set_idle();
            }
        });
    }

    pub fn hide_upload_progress(&self) {
        self.set_idle();
    }

    // ── Idle timer ───────────────────────────────────────────────────

    fn update_idle_label(&self) {
        let imp = self.imp();
        let label = imp.idle_label();

        let Some(synced_at) = imp.last_synced_at.get() else {
            label.set_text("Waiting for sync...");
            return;
        };

        let elapsed = chrono::Utc::now().timestamp() - synced_at;
        let text = if elapsed < 10 {
            "Synced just now".to_string()
        } else if elapsed < 60 {
            format!("Synced {}s ago", elapsed)
        } else if elapsed < 3600 {
            format!("Synced {}m ago", elapsed / 60)
        } else {
            format!("Synced {}h ago", elapsed / 3600)
        };
        label.set_text(&text);
    }

    fn start_idle_timer(&self) {
        let imp = self.imp();

        if let Some(id) = imp.sync_timer.borrow_mut().take() {
            id.remove();
        }

        let obj_weak = self.downgrade();
        let id = glib::timeout_add_local(std::time::Duration::from_secs(10), move || {
            let Some(obj) = obj_weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if obj.imp().current_state.get() == imp::StatusState::Idle {
                obj.update_idle_label();
            }
            glib::ControlFlow::Continue
        });
        *imp.sync_timer.borrow_mut() = Some(id);
    }
}

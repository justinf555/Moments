pub mod route;

use std::cell::Cell;
use std::cell::RefCell;

use gettextrs::gettext;
use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use adw::prelude::*;
use tracing::debug;

use route::ROUTES;

mod imp {
    use super::*;
    use std::cell::OnceCell;
    use std::rc::Rc;

    pub struct MomentsSidebar {
        pub sidebar: OnceCell<adw::Sidebar>,
        pub pinned_section: OnceCell<adw::SidebarSection>,
        /// Album IDs for pinned items, in display order.
        pub pinned_ids: RefCell<Vec<String>>,
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
                pinned_section: OnceCell::new(),
                pinned_ids: RefCell::new(Vec::new()),
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
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsSidebar {
        const NAME: &'static str = "MomentsSidebar";
        type Type = super::MomentsSidebar;
        type ParentType = adw::NavigationPage;
    }

    impl ObjectImpl for MomentsSidebar {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.set_title("Moments");

            let toolbar_view = adw::ToolbarView::new();

            // ── Sidebar header bar ───────────────────────────────────────
            let header = adw::HeaderBar::new();

            let menu_button = gtk::MenuButton::builder()
                .primary(true)
                .icon_name("open-menu-symbolic")
                .tooltip_text(&gettext("Main Menu"))
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

            toolbar_view.add_top_bar(&header);

            // ── AdwSidebar ──────────────────────────────────────────────
            let sidebar = adw::Sidebar::new();
            let section = adw::SidebarSection::new();

            for (i, route) in ROUTES.iter().enumerate() {
                let mut builder = adw::SidebarItem::builder()
                    .title(&gettext(route.label))
                    .icon_name(route.icon);

                // Trash item gets a badge suffix showing the item count.
                if route.id == "trash" {
                    let badge = gtk::Label::new(None);
                    badge.add_css_class("sidebar-badge");
                    badge.set_visible(false);
                    builder = builder.suffix(&badge);
                    let _ = self.trash_badge.set(badge);
                }

                section.append(builder.build());
            }

            sidebar.append(section);

            // Pinned albums section — hidden when empty.
            let pinned_section = adw::SidebarSection::new();
            pinned_section.set_title(Some(&gettext("Pinned")));

            // Context menu for pinned items: "Unpin from sidebar".
            let unpin_menu = gio::Menu::new();
            unpin_menu.append(Some(&gettext("Unpin from Sidebar")), Some("sidebar.unpin"));
            pinned_section.set_menu_model(Some(&unpin_menu));

            sidebar.append(pinned_section.clone());
            let _ = self.pinned_section.set(pinned_section.clone());

            // Track which pinned item was right-clicked for the unpin action.
            let menu_target_index: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
            {
                let mti = Rc::clone(&menu_target_index);
                let ps = pinned_section.clone();
                sidebar.connect_setup_menu(move |_, item| {
                    // Determine if this item is in the pinned section.
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

            // Unpin action — removes the right-clicked pinned item.
            let unpin_action = gio::SimpleAction::new("unpin", None);
            {
                let mti = Rc::clone(&menu_target_index);
                let obj_weak = obj.downgrade();
                unpin_action.connect_activate(move |_, _| {
                    let Some(index) = mti.get() else { return };
                    let Some(sidebar) = obj_weak.upgrade() else { return };
                    let ids = sidebar.imp().pinned_ids.borrow();
                    if let Some(album_id) = ids.get(index as usize).cloned() {
                        drop(ids);
                        // Get settings from the app.
                        let app = crate::application::MomentsApplication::default();
                        if let Some(settings) = app.imp().settings.get() {
                            sidebar.unpin_album(&album_id, settings);
                        }
                    }
                });
            }

            let sidebar_action_group = gio::SimpleActionGroup::new();
            sidebar_action_group.add_action(&unpin_action);
            toolbar_view.insert_action_group("sidebar", Some(&sidebar_action_group));

            toolbar_view.set_content(Some(&sidebar));

            // ── Status bar (bottom bar of the BottomSheet) ───────────────
            let bar_stack = gtk::Stack::new();
            bar_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
            bar_stack.set_transition_duration(200);

            // Idle page: "Synced X ago" or "Waiting for sync..."
            let idle_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            idle_box.set_margin_start(12);
            idle_box.set_margin_end(12);
            idle_box.set_margin_top(8);
            idle_box.set_margin_bottom(8);
            let idle_icon = gtk::Image::from_icon_name("object-select-symbolic");
            idle_icon.add_css_class("dim-label");
            idle_box.append(&idle_icon);
            let idle_label = gtk::Label::new(Some("Waiting for sync..."));
            idle_label.set_hexpand(true);
            idle_label.set_xalign(0.0);
            idle_label.add_css_class("dim-label");
            idle_label.add_css_class("caption");
            idle_box.append(&idle_label);
            bar_stack.add_named(&idle_box, Some("idle"));

            // Sync page: "Syncing..."
            let sync_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            sync_box.set_margin_start(12);
            sync_box.set_margin_end(12);
            sync_box.set_margin_top(8);
            sync_box.set_margin_bottom(8);
            let sync_icon = gtk::Image::from_icon_name("view-refresh-symbolic");
            sync_box.append(&sync_icon);
            let sync_label = gtk::Label::new(Some("Syncing..."));
            sync_label.set_hexpand(true);
            sync_label.set_xalign(0.0);
            sync_label.add_css_class("caption");
            sync_box.append(&sync_label);
            bar_stack.add_named(&sync_box, Some("sync"));

            // Thumbnails page: "Thumbnails X/Y"
            let thumb_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            thumb_box.set_margin_start(12);
            thumb_box.set_margin_end(12);
            thumb_box.set_margin_top(8);
            thumb_box.set_margin_bottom(8);
            let thumb_icon = gtk::Image::from_icon_name("folder-download-symbolic");
            thumb_box.append(&thumb_icon);
            let thumb_label = gtk::Label::new(Some("Downloading thumbnails..."));
            thumb_label.set_hexpand(true);
            thumb_label.set_xalign(0.0);
            thumb_label.add_css_class("caption");
            thumb_box.append(&thumb_label);
            bar_stack.add_named(&thumb_box, Some("thumbnails"));

            // Upload page: "Uploading X/Y"
            let upload_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            upload_box.set_margin_start(12);
            upload_box.set_margin_end(12);
            upload_box.set_margin_top(12);
            upload_box.set_margin_bottom(16);
            let upload_icon = gtk::Image::from_icon_name("go-up-symbolic");
            upload_box.append(&upload_icon);
            let upload_label = gtk::Label::new(Some("Uploading..."));
            upload_label.set_hexpand(true);
            upload_label.set_xalign(0.0);
            upload_label.add_css_class("caption");
            upload_box.append(&upload_label);
            bar_stack.add_named(&upload_box, Some("upload"));

            // Complete page: "Upload Complete"
            let complete_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            complete_box.set_margin_start(12);
            complete_box.set_margin_end(12);
            complete_box.set_margin_top(8);
            complete_box.set_margin_bottom(8);
            let complete_icon = gtk::Image::from_icon_name("object-select-symbolic");
            complete_box.append(&complete_icon);
            let complete_label = gtk::Label::new(Some("Import complete"));
            complete_label.set_hexpand(true);
            complete_label.set_xalign(0.0);
            complete_label.add_css_class("caption");
            complete_box.append(&complete_label);
            bar_stack.add_named(&complete_box, Some("complete"));

            // ── Upload detail sheet (expanded view) ──────────────────────
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

            // ── Bottom sheet ─────────────────────────────────────────────
            let bottom_sheet = adw::BottomSheet::new();
            bottom_sheet.set_content(Some(&toolbar_view));
            bottom_sheet.set_sheet(Some(&sheet_box));
            bottom_sheet.set_bottom_bar(Some(&bar_stack));
            bottom_sheet.set_open(false);
            bottom_sheet.set_show_drag_handle(false);
            bottom_sheet.set_can_open(false);
            bottom_sheet.set_modal(false);
            bottom_sheet.set_full_width(true);
            bottom_sheet.set_reveal_bottom_bar(true);

            obj.set_child(Some(&bottom_sheet));

            self.sidebar.set(sidebar).expect("set once in constructed");
            self.bottom_sheet.set(bottom_sheet).expect("set once in constructed");
            self.progress_label.set(progress_label).expect("set once in constructed");
            self.progress_bar.set(progress_bar).expect("set once in constructed");
            self.detail_label.set(detail_label).expect("set once in constructed");
            self.bar_stack.set(bar_stack).expect("set once in constructed");
            self.idle_label.set(idle_label).expect("set once in constructed");
            self.sync_label.set(sync_label).expect("set once in constructed");
            self.thumb_label.set(thumb_label).expect("set once in constructed");
            self.upload_label.set(upload_label).expect("set once in constructed");
            self.complete_label.set(complete_label).expect("set once in constructed");
        }
    }

    impl WidgetImpl for MomentsSidebar {}
    impl adw::subclass::prelude::NavigationPageImpl for MomentsSidebar {}
}

glib::wrapper! {
    pub struct MomentsSidebar(ObjectSubclass<imp::MomentsSidebar>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MomentsSidebar {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Subscribe to sync, import, thumbnail, and trash count events.
    pub fn subscribe_to_bus(&self) {
        let weak = self.downgrade();
        crate::event_bus::subscribe(move |event| {
            let Some(sidebar) = weak.upgrade() else { return };
            match event {
                crate::app_event::AppEvent::SyncStarted => {
                    sidebar.show_sync_started();
                }
                crate::app_event::AppEvent::SyncProgress { assets, people, faces } => {
                    sidebar.show_sync_progress(*assets, *people, *faces);
                }
                crate::app_event::AppEvent::SyncComplete { assets, .. } => {
                    sidebar.show_sync_complete(*assets);
                }
                crate::app_event::AppEvent::ThumbnailDownloadProgress { completed, total } => {
                    sidebar.show_thumbnail_progress(*completed, *total);
                }
                crate::app_event::AppEvent::ThumbnailDownloadsComplete { total } => {
                    sidebar.show_thumbnails_complete(*total);
                }
                crate::app_event::AppEvent::ImportProgress { current, total, imported, skipped, failed } => {
                    sidebar.show_upload_progress(*current, *total, *imported, *skipped, *failed);
                }
                crate::app_event::AppEvent::ImportComplete { summary } => {
                    sidebar.show_upload_complete(summary);
                }
                // Dynamic trash count updates.
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
    }

    /// Connect a callback that fires when the user activates a sidebar item.
    ///
    /// System routes (index 0–5) map to `ROUTES[index].id`.
    /// Pinned album items (index 6+) map to `"album:{album_id}"`.
    pub fn connect_route_selected<F: Fn(&str) + 'static>(&self, f: F) {
        let sidebar = self.imp().sidebar.get().unwrap().clone();
        let weak = self.downgrade();
        sidebar.connect_activated(move |_, index| {
            let system_count = ROUTES.len() as u32;
            if index < system_count {
                if let Some(route) = ROUTES.get(index as usize) {
                    debug!(route = %route.id, "sidebar route selected");
                    f(route.id);
                }
            } else {
                // Pinned album item.
                let pinned_index = (index - system_count) as usize;
                if let Some(sidebar) = weak.upgrade() {
                    let ids = sidebar.imp().pinned_ids.borrow();
                    if let Some(album_id) = ids.get(pinned_index) {
                        let route = format!("album:{album_id}");
                        debug!(route = %route, "pinned album selected");
                        f(&route);
                    }
                }
            }
        });
    }

    /// Pre-select the first item (Photos) so the shell always has an active route.
    pub fn select_first(&self) {
        self.imp().sidebar.get().unwrap().set_selected(0);
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

    /// Maximum number of pinned albums.
    const MAX_PINNED: usize = 5;

    /// Load pinned albums from GSettings and populate the Pinned section.
    ///
    /// Called once at startup after the library is available. `albums` is
    /// the full album list so we can resolve names for the sidebar items.
    pub fn load_pinned_albums(
        &self,
        settings: &gtk::gio::Settings,
        albums: &[(String, String)], // (id, name)
    ) {
        let imp = self.imp();
        let ids: Vec<String> = settings
            .strv("pinned-album-ids")
            .iter()
            .map(|s| s.to_string())
            .collect();

        let section = imp.pinned_section.get().unwrap();
        section.remove_all();

        let mut valid_ids = Vec::new();
        for id in &ids {
            // Find the album name — skip if the album was deleted.
            if let Some((_, name)) = albums.iter().find(|(aid, _)| aid == id) {
                let item = adw::SidebarItem::builder()
                    .title(name)
                    .icon_name("folder-symbolic")
                    .build();
                section.append(item);
                valid_ids.push(id.clone());
            }
        }
        *imp.pinned_ids.borrow_mut() = valid_ids;
    }

    /// Pin an album to the sidebar. Returns false if already pinned or at limit.
    pub fn pin_album(&self, album_id: &str, album_name: &str, settings: &gtk::gio::Settings) -> bool {
        let imp = self.imp();
        let mut ids = imp.pinned_ids.borrow_mut();

        if ids.len() >= Self::MAX_PINNED || ids.contains(&album_id.to_string()) {
            return false;
        }

        ids.push(album_id.to_string());

        let section = imp.pinned_section.get().unwrap();
        let item = adw::SidebarItem::builder()
            .title(album_name)
            .icon_name("folder-symbolic")
            .build();
        section.append(item);

        // Persist.
        let strv: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        settings.set_strv("pinned-album-ids", strv).ok();

        debug!(album_id = %album_id, name = %album_name, "album pinned to sidebar");
        true
    }

    /// Unpin an album from the sidebar.
    pub fn unpin_album(&self, album_id: &str, settings: &gtk::gio::Settings) {
        let imp = self.imp();
        let mut ids = imp.pinned_ids.borrow_mut();

        if let Some(pos) = ids.iter().position(|id| id == album_id) {
            ids.remove(pos);
            let section = imp.pinned_section.get().unwrap();
            if let Some(item) = section.item(pos as u32) {
                section.remove(&item);
            }

            // Persist.
            let strv: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
            settings.set_strv("pinned-album-ids", strv).ok();

            debug!(album_id = %album_id, "album unpinned from sidebar");
        }
    }

    /// Number of currently pinned albums.
    pub fn pinned_count(&self) -> usize {
        self.imp().pinned_ids.borrow().len()
    }

    /// Whether the given album is currently pinned.
    pub fn is_pinned(&self, album_id: &str) -> bool {
        self.imp().pinned_ids.borrow().iter().any(|id| id == album_id)
    }

    /// Update the Trash badge with the current count.
    fn update_trash_badge(&self) {
        let imp = self.imp();
        if let Some(badge) = imp.trash_badge.get() {
            let count = imp.trash_count.get();
            if count > 0 {
                badge.set_label(&count.to_string());
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
            if let Some(stack) = imp.bar_stack.get() {
                stack.set_visible_child_name(page);
            }
            if state != imp::StatusState::Upload {
                if let Some(sheet) = imp.bottom_sheet.get() {
                    sheet.set_can_open(false);
                    sheet.set_open(false);
                }
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
        if let Some(label) = imp.sync_label.get() {
            label.set_text("Syncing...");
        }
        self.set_status(imp::StatusState::Sync, "sync");
    }

    pub fn show_sync_progress(&self, assets: usize, people: usize, faces: usize) {
        let imp = self.imp();
        let total = assets + people + faces;
        if let Some(label) = imp.sync_label.get() {
            label.set_text(&format!("Syncing... {total} items"));
        }
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

    pub fn show_thumbnail_progress(&self, completed: usize, total: usize) {
        let imp = self.imp();
        if imp.current_state.get() == imp::StatusState::Idle {
            return;
        }
        if let Some(label) = imp.thumb_label.get() {
            label.set_text(&format!("Thumbnails {completed}/{total}"));
        }
        self.set_status(imp::StatusState::Thumbnails, "thumbnails");
    }

    pub fn show_thumbnails_complete(&self, _total: usize) {
        self.set_idle();
    }

    pub fn show_upload_progress(&self, current: usize, total: usize, imported: usize, skipped: usize, failed: usize) {
        let imp = self.imp();
        if let Some(label) = imp.upload_label.get() {
            label.set_text(&format!("Uploading {current}/{total}"));
        }
        if let Some(label) = imp.progress_label.get() {
            label.set_text(&format!("Uploading {current} of {total}"));
        }
        if let Some(bar) = imp.progress_bar.get() {
            if total > 0 {
                bar.set_fraction(current as f64 / total as f64);
            }
        }
        let mut detail = format!("{imported} imported");
        if skipped > 0 {
            detail.push_str(&format!(", {skipped} skipped"));
        }
        if failed > 0 {
            detail.push_str(&format!(", {failed} failed"));
        }
        if let Some(label) = imp.detail_label.get() {
            label.set_text(&detail);
        }
        if let Some(sheet) = imp.bottom_sheet.get() {
            if !sheet.is_open() {
                sheet.set_can_open(true);
                sheet.set_open(true);
            }
        }
        self.set_status(imp::StatusState::Upload, "upload");
    }

    pub fn show_upload_complete(&self, summary: &crate::library::import::ImportSummary) {
        let imp = self.imp();

        let mut bar_text = format!("{} imported", summary.imported);
        if summary.skipped_duplicates > 0 {
            bar_text.push_str(&format!(", {} skipped", summary.skipped_duplicates));
        }
        if summary.failed > 0 {
            bar_text.push_str(&format!(", {} failed", summary.failed));
        }

        if let Some(label) = imp.complete_label.get() {
            label.set_text("Upload Complete");
        }
        if let Some(label) = imp.progress_label.get() {
            label.set_text(&bar_text);
        }
        if let Some(bar) = imp.progress_bar.get() {
            bar.set_fraction(1.0);
        }
        if let Some(label) = imp.detail_label.get() {
            label.set_text(&bar_text);
        }

        if let Some(sheet) = imp.bottom_sheet.get() {
            sheet.set_open(false);
        }

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
        let Some(label) = imp.idle_label.get() else { return };

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

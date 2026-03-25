pub mod route;
pub mod row;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use adw::prelude::*;
use tracing::debug;

use route::{TOP_ROUTES, BOTTOM_ROUTES};
use row::MomentsSidebarRow;

/// Stored callbacks for album row right-click menus.
struct AlbumContextMenu {
    on_rename: std::rc::Rc<dyn Fn(String, String)>,
    on_delete: std::rc::Rc<dyn Fn(String, String)>,
}

mod imp {
    use super::*;
    use std::cell::OnceCell;

    pub struct MomentsSidebar {
        pub list_box: OnceCell<gtk::ListBox>,
        /// Maps album_id → ListBoxRow for dynamic add/remove.
        pub album_rows: RefCell<HashMap<String, gtk::ListBoxRow>>,
        /// The non-selectable header row for the Albums section.
        pub albums_header: OnceCell<gtk::ListBoxRow>,
        /// The separator row before Trash (albums are inserted before this).
        pub bottom_separator: OnceCell<gtk::ListBoxRow>,
        /// The "+" button for creating albums.
        pub add_button: OnceCell<gtk::Button>,
        /// Stored context menu callbacks (set once via set_album_context_callbacks).
        pub(super) context_menu: RefCell<Option<AlbumContextMenu>>,

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
                list_box: OnceCell::new(),
                album_rows: RefCell::new(HashMap::new()),
                albums_header: OnceCell::new(),
                bottom_separator: OnceCell::new(),
                add_button: OnceCell::new(),
                context_menu: RefCell::new(None),
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

            // Import button (LHS).
            let import_button = gtk::Button::builder()
                .icon_name("document-send-symbolic")
                .tooltip_text("Import Photos")
                .action_name("app.import")
                .build();
            import_button.add_css_class("flat");
            header.pack_start(&import_button);

            // Hamburger menu (RHS).
            let menu_button = gtk::MenuButton::builder()
                .primary(true)
                .icon_name("open-menu-symbolic")
                .tooltip_text("Main Menu")
                .build();
            let menu = gio::Menu::new();
            let section = gio::Menu::new();
            section.append(Some("_Preferences"), Some("app.preferences"));
            section.append(Some("_Keyboard Shortcuts"), Some("app.shortcuts"));
            section.append(Some("_About Moments"), Some("app.about"));
            menu.append_section(None, &section);
            menu_button.set_menu_model(Some(&menu));
            header.pack_end(&menu_button);

            toolbar_view.add_top_bar(&header);

            // ── Route list ───────────────────────────────────────────────
            let list_box = gtk::ListBox::new();
            list_box.set_selection_mode(gtk::SelectionMode::Single);
            list_box.add_css_class("navigation-sidebar");

            // Top routes (Photos, Favorites, Recent Imports, People).
            for route in TOP_ROUTES {
                let row = MomentsSidebarRow::new(route.id, route.label, route.icon);
                let list_row = gtk::ListBoxRow::new();
                list_row.set_child(Some(&row));
                list_box.append(&list_row);
            }

            // Albums header row.
            let (header_row, add_button) = Self::make_albums_header();
            list_box.append(&header_row);
            self.albums_header
                .set(header_row)
                .expect("albums_header set once");
            self.add_button
                .set(add_button)
                .expect("add_button set once");

            // Bottom spacer (albums are inserted before this).
            let spacer = gtk::ListBoxRow::new();
            spacer.set_selectable(false);
            spacer.set_activatable(false);
            spacer.set_visible(false);
            list_box.append(&spacer);
            self.bottom_separator
                .set(spacer)
                .expect("bottom_separator set once");

            // Bottom routes (Trash).
            for (i, route) in BOTTOM_ROUTES.iter().enumerate() {
                let row = MomentsSidebarRow::new(route.id, route.label, route.icon);
                let list_row = gtk::ListBoxRow::new();
                list_row.set_child(Some(&row));
                if i == 0 {
                    list_row.set_margin_top(12);
                }
                list_box.append(&list_row);
            }

            let scrolled = gtk::ScrolledWindow::new();
            scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
            scrolled.set_vexpand(true);
            scrolled.set_child(Some(&list_box));

            toolbar_view.set_content(Some(&scrolled));

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

            // Complete page: "✓ X imported"
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
            bottom_sheet.set_show_drag_handle(true);
            bottom_sheet.set_modal(false);
            bottom_sheet.set_full_width(true);
            // Always visible — shows status at all times.
            bottom_sheet.set_reveal_bottom_bar(true);

            obj.set_child(Some(&bottom_sheet));

            self.list_box.set(list_box).unwrap();
            let _ = self.bottom_sheet.set(bottom_sheet);
            let _ = self.progress_label.set(progress_label);
            let _ = self.progress_bar.set(progress_bar);
            let _ = self.detail_label.set(detail_label);
            let _ = self.bar_stack.set(bar_stack);
            let _ = self.idle_label.set(idle_label);
            let _ = self.sync_label.set(sync_label);
            let _ = self.thumb_label.set(thumb_label);
            let _ = self.upload_label.set(upload_label);
            let _ = self.complete_label.set(complete_label);
        }
    }

    impl imp::MomentsSidebar {
        /// Create the "Albums" header row with a "+" button.
        fn make_albums_header() -> (gtk::ListBoxRow, gtk::Button) {
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            hbox.set_margin_start(12);
            hbox.set_margin_end(6);
            hbox.set_margin_top(2);
            hbox.set_margin_bottom(2);

            let label = gtk::Label::new(Some("Albums"));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.add_css_class("dim-label");
            label.add_css_class("caption-heading");
            hbox.append(&label);

            let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
            add_btn.add_css_class("flat");
            add_btn.set_tooltip_text(Some("New album"));
            hbox.append(&add_btn);

            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&hbox));
            row.set_selectable(false);
            row.set_activatable(false);
            row.set_margin_top(12);

            (row, add_btn)
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

    /// Connect a callback that fires when the user selects a row.
    pub fn connect_route_selected<F: Fn(&str) + 'static>(&self, f: F) {
        let list_box = self.imp().list_box.get().unwrap().clone();
        list_box.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let Some(child) = row.child() else { return };
            let Some(sidebar_row) = child.downcast_ref::<MomentsSidebarRow>() else {
                return;
            };
            let id = sidebar_row.route_id().to_owned();
            debug!(route = %id, "sidebar route selected");
            f(&id);
        });
    }

    /// Pre-select the first row so the shell always has an active route.
    pub fn select_first(&self) {
        let list_box = self.imp().list_box.get().unwrap();
        if let Some(first) = list_box.row_at_index(0) {
            list_box.select_row(Some(&first));
        }
    }

    /// Populate all album rows at startup.
    pub fn set_albums(&self, albums: &[(String, String)]) {
        let imp = self.imp();
        let mut album_rows = imp.album_rows.borrow_mut();
        let list_box = imp.list_box.get().unwrap();
        for (_, row) in album_rows.drain() {
            list_box.remove(&row);
        }
        drop(album_rows);

        for (id, name) in albums {
            self.add_album(id, name);
        }
    }

    /// Add a single album row to the Albums section.
    pub fn add_album(&self, album_id: &str, name: &str) {
        if self.imp().album_rows.borrow().contains_key(album_id) {
            self.rename_album(album_id, name);
            return;
        }

        let imp = self.imp();
        let list_box = imp.list_box.get().unwrap();
        let bottom_sep = imp.bottom_separator.get().unwrap();

        let route_id = format!("album:{album_id}");
        let sidebar_row = MomentsSidebarRow::new(&route_id, name, "folder-symbolic");
        let list_row = gtk::ListBoxRow::new();
        list_row.set_child(Some(&sidebar_row));

        list_box.insert(&list_row, bottom_sep.index());

        self.attach_row_context_menu(&list_row, album_id, name);

        imp.album_rows
            .borrow_mut()
            .insert(album_id.to_owned(), list_row);
    }

    /// Remove an album row by album ID.
    pub fn remove_album(&self, album_id: &str) {
        let imp = self.imp();
        let list_box = imp.list_box.get().unwrap();

        if let Some(row) = imp.album_rows.borrow_mut().remove(album_id) {
            let is_selected = list_box
                .selected_row()
                .map(|sel| sel == row)
                .unwrap_or(false);

            list_box.remove(&row);

            if is_selected {
                self.select_first();
            }
        }
    }

    /// Update an album row's displayed name.
    pub fn rename_album(&self, album_id: &str, name: &str) {
        let imp = self.imp();
        let album_rows = imp.album_rows.borrow();
        if let Some(row) = album_rows.get(album_id) {
            if let Some(child) = row.child() {
                if let Some(sidebar_row) = child.downcast_ref::<MomentsSidebarRow>() {
                    sidebar_row.set_label_text(name);
                }
            }
        }
    }

    /// Connect a callback for the "+" (new album) button.
    pub fn connect_album_add_clicked<F: Fn() + 'static>(&self, f: F) {
        if let Some(btn) = self.imp().add_button.get() {
            btn.connect_clicked(move |_| f());
        }
    }

    /// Store callbacks for album context menu actions.
    pub fn set_album_context_callbacks(
        &self,
        on_rename: impl Fn(String, String) + 'static,
        on_delete: impl Fn(String, String) + 'static,
    ) {
        *self.imp().context_menu.borrow_mut() = Some(AlbumContextMenu {
            on_rename: std::rc::Rc::new(on_rename),
            on_delete: std::rc::Rc::new(on_delete),
        });
    }

    /// Attach a right-click gesture to an individual album row.
    fn attach_row_context_menu(&self, list_row: &gtk::ListBoxRow, album_id: &str, name: &str) {
        let ctx = self.imp().context_menu.borrow();
        let Some(ctx) = ctx.as_ref() else { return };

        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);

        let on_rename = ctx.on_rename.clone();
        let on_delete = ctx.on_delete.clone();
        let aid = album_id.to_owned();
        let aname = name.to_owned();
        let row_weak = list_row.downgrade();

        gesture.connect_pressed(move |gesture, _, x, _y| {
            let Some(row) = row_weak.upgrade() else { return };

            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
            vbox.add_css_class("menu");

            let rename_btn = gtk::Button::with_label("Rename");
            rename_btn.add_css_class("flat");
            vbox.append(&rename_btn);

            let delete_btn = gtk::Button::with_label("Delete");
            delete_btn.add_css_class("flat");
            delete_btn.add_css_class("error");
            vbox.append(&delete_btn);

            let popover = gtk::Popover::new();
            popover.set_child(Some(&vbox));
            popover.set_parent(&row);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, 0, 1, 1)));
            popover.set_has_arrow(true);

            let rename_cb = on_rename.clone();
            let aid_r = aid.clone();
            let aname_r = aname.clone();
            let pop_weak = popover.downgrade();
            rename_btn.connect_clicked(move |_| {
                if let Some(p) = pop_weak.upgrade() {
                    p.popdown();
                }
                rename_cb(aid_r.clone(), aname_r.clone());
            });

            let delete_cb = on_delete.clone();
            let aid_d = aid.clone();
            let aname_d = aname.clone();
            let pop_weak = popover.downgrade();
            delete_btn.connect_clicked(move |_| {
                if let Some(p) = pop_weak.upgrade() {
                    p.popdown();
                }
                delete_cb(aid_d.clone(), aname_d.clone());
            });

            popover.connect_closed(move |p| {
                p.unparent();
            });

            popover.popup();
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });

        list_row.add_controller(gesture);
    }

    // ── Status bar methods ───────────────────────────────────────────

    /// Switch the status bar to a named state, respecting priority.
    ///
    /// Higher-priority states (upload) can't be overridden by lower
    /// (sync, thumbnails). Setting idle always succeeds.
    fn set_status(&self, state: imp::StatusState, page: &str) {
        let imp = self.imp();
        let current = imp.current_state.get();

        // Allow transition if: same or higher priority, or resetting to idle.
        if state >= current || state == imp::StatusState::Idle {
            imp.current_state.set(state);
            if let Some(stack) = imp.bar_stack.get() {
                stack.set_visible_child_name(page);
            }
            // Only allow expanding the sheet during uploads.
            if let Some(sheet) = imp.bottom_sheet.get() {
                let can_expand = state == imp::StatusState::Upload;
                sheet.set_show_drag_handle(can_expand);
                if !can_expand && sheet.is_open() {
                    sheet.set_open(false);
                }
            }
        }
    }

    /// Set idle state with "Synced X ago" label.
    pub fn set_idle(&self) {
        self.set_status(imp::StatusState::Idle, "idle");
        self.update_idle_label();
        self.start_idle_timer();
    }

    /// Show sync started status.
    pub fn show_sync_started(&self) {
        let imp = self.imp();
        if let Some(label) = imp.sync_label.get() {
            label.set_text("Syncing...");
        }
        self.set_status(imp::StatusState::Sync, "sync");
    }

    /// Show sync progress.
    pub fn show_sync_progress(&self, assets: usize, people: usize, faces: usize) {
        let imp = self.imp();
        let total = assets + people + faces;
        if let Some(label) = imp.sync_label.get() {
            label.set_text(&format!("Syncing... {total} items"));
        }
        self.set_status(imp::StatusState::Sync, "sync");
    }

    /// Show sync complete — update sync timestamp and schedule idle.
    ///
    /// Thumbnail downloads may still be in-flight after the sync stream
    /// finishes. We schedule a delayed transition to idle that fires
    /// after 3 seconds — if thumbnail progress events arrive in the
    /// meantime, they'll take priority and the idle will be rescheduled
    /// on the next sync complete.
    pub fn show_sync_complete(&self, _assets: usize) {
        let imp = self.imp();
        imp.last_synced_at.set(Some(chrono::Utc::now().timestamp()));

        // If already idle or only thumbnails running, go to idle now.
        let current = imp.current_state.get();
        if current == imp::StatusState::Idle || current == imp::StatusState::Sync {
            self.set_idle();
        } else {
            // Thumbnails still running — schedule delayed idle.
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

    /// Show thumbnail download progress.
    ///
    /// Suppressed if sync has already completed (last_synced_at is set)
    /// and we've already transitioned to idle — avoids re-triggering
    /// the thumbnail state from straggler downloads after sync ends.
    pub fn show_thumbnail_progress(&self, completed: usize, total: usize) {
        let imp = self.imp();
        // Only show thumbnail state while sync is actively running.
        if imp.current_state.get() == imp::StatusState::Idle {
            return;
        }
        if let Some(label) = imp.thumb_label.get() {
            label.set_text(&format!("Thumbnails {completed}/{total}"));
        }
        self.set_status(imp::StatusState::Thumbnails, "thumbnails");
    }

    /// Show thumbnails complete — transition to idle.
    pub fn show_thumbnails_complete(&self, _total: usize) {
        self.set_idle();
    }

    /// Show upload progress in the sidebar bottom sheet.
    pub fn show_upload_progress(&self, current: usize, total: usize) {
        let imp = self.imp();
        if let Some(label) = imp.upload_label.get() {
            label.set_text(&format!("{current}/{total}"));
        }
        if let Some(label) = imp.progress_label.get() {
            label.set_text(&format!("Uploading {current} of {total}"));
        }
        if let Some(bar) = imp.progress_bar.get() {
            if total > 0 {
                bar.set_fraction(current as f64 / total as f64);
            }
        }
        self.set_status(imp::StatusState::Upload, "upload");
    }

    /// Show upload complete summary, then auto-revert to idle after 5 seconds.
    pub fn show_upload_complete(&self, summary: &crate::library::import::ImportSummary) {
        let imp = self.imp();

        // Build summary text.
        let mut bar_text = format!("{} imported", summary.imported);
        if summary.skipped_duplicates > 0 {
            bar_text.push_str(&format!(", {} skipped", summary.skipped_duplicates));
        }
        if summary.failed > 0 {
            bar_text.push_str(&format!(", {} failed", summary.failed));
        }

        if let Some(label) = imp.complete_label.get() {
            label.set_text(&bar_text);
        }
        if let Some(label) = imp.progress_label.get() {
            label.set_text("Upload Complete");
        }
        if let Some(bar) = imp.progress_bar.get() {
            bar.set_fraction(1.0);
        }
        if let Some(label) = imp.detail_label.get() {
            label.set_text(&bar_text);
        }

        // Close expanded sheet.
        if let Some(sheet) = imp.bottom_sheet.get() {
            sheet.set_open(false);
        }

        self.set_status(imp::StatusState::Complete, "complete");

        // Auto-revert to idle after 5 seconds.
        let obj_weak = self.downgrade();
        glib::timeout_add_local_once(std::time::Duration::from_secs(5), move || {
            if let Some(obj) = obj_weak.upgrade() {
                obj.set_idle();
            }
        });
    }

    /// Hide the upload progress bottom bar (legacy — now reverts to idle).
    pub fn hide_upload_progress(&self) {
        self.set_idle();
    }

    // ── Idle timer ───────────────────────────────────────────────────

    /// Update the idle label with "Synced X ago" or "Waiting for sync...".
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

    /// Start a 10-second timer to keep the "Synced X ago" label current.
    fn start_idle_timer(&self) {
        let imp = self.imp();

        // Cancel any existing timer.
        if let Some(id) = imp.sync_timer.borrow_mut().take() {
            id.remove();
        }

        let obj_weak = self.downgrade();
        let id = glib::timeout_add_local(std::time::Duration::from_secs(10), move || {
            let Some(obj) = obj_weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            // Only update if we're still in idle state.
            if obj.imp().current_state.get() == imp::StatusState::Idle {
                obj.update_idle_label();
            }
            glib::ControlFlow::Continue
        });
        *imp.sync_timer.borrow_mut() = Some(id);
    }
}

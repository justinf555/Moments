pub mod route;
pub mod row;

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::{glib, prelude::*, subclass::prelude::*};
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
        /// Bottom sheet for upload progress.
        pub bottom_sheet: OnceCell<adw::BottomSheet>,
        /// Progress widgets inside the bottom sheet.
        pub progress_label: OnceCell<gtk::Label>,
        pub progress_bar: OnceCell<gtk::ProgressBar>,
        pub detail_label: OnceCell<gtk::Label>,
        pub bar_label: OnceCell<gtk::Label>,
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
                bar_label: OnceCell::new(),
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
            let header = adw::HeaderBar::new();
            toolbar_view.add_top_bar(&header);

            let list_box = gtk::ListBox::new();
            list_box.set_selection_mode(gtk::SelectionMode::Single);
            list_box.add_css_class("navigation-sidebar");

            // ── Top routes (Photos, Favorites, Recent Imports) ──────────
            for route in TOP_ROUTES {
                let row = MomentsSidebarRow::new(route.id, route.label, route.icon);
                let list_row = gtk::ListBoxRow::new();
                list_row.set_child(Some(&row));
                list_box.append(&list_row);
            }

            // ── Albums header row ───────────────────────────────────────
            let (header_row, add_button) = Self::make_albums_header();
            list_box.append(&header_row);
            self.albums_header
                .set(header_row)
                .expect("albums_header set once");
            self.add_button
                .set(add_button)
                .expect("add_button set once");

            // ── Bottom spacer (albums are inserted before this) ─────────
            // Non-visible spacer row used as an insertion anchor.
            let spacer = gtk::ListBoxRow::new();
            spacer.set_selectable(false);
            spacer.set_activatable(false);
            spacer.set_visible(false);
            list_box.append(&spacer);
            self.bottom_separator
                .set(spacer)
                .expect("bottom_separator set once");

            // ── Bottom routes (Trash) ───────────────────────────────────
            for (i, route) in BOTTOM_ROUTES.iter().enumerate() {
                let row = MomentsSidebarRow::new(route.id, route.label, route.icon);
                let list_row = gtk::ListBoxRow::new();
                list_row.set_child(Some(&row));
                if i == 0 {
                    list_row.set_margin_top(12); // visual gap from Albums section
                }
                list_box.append(&list_row);
            }

            let scrolled = gtk::ScrolledWindow::new();
            scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
            scrolled.set_vexpand(true);
            scrolled.set_child(Some(&list_box));

            toolbar_view.set_content(Some(&scrolled));

            // ── Upload progress bottom sheet ──────────────────────────────
            // Bottom bar: compact one-line summary shown during uploads.
            let bar_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            bar_box.set_margin_start(12);
            bar_box.set_margin_end(12);
            bar_box.set_margin_top(8);
            bar_box.set_margin_bottom(8);

            let bar_icon = gtk::Image::from_icon_name("go-up-symbolic");
            bar_box.append(&bar_icon);

            let bar_label = gtk::Label::new(Some("Uploading..."));
            bar_label.set_hexpand(true);
            bar_label.set_xalign(0.0);
            bar_box.append(&bar_label);

            // Sheet: expanded detail view.
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

            let bottom_sheet = adw::BottomSheet::new();
            bottom_sheet.set_content(Some(&toolbar_view));
            bottom_sheet.set_sheet(Some(&sheet_box));
            bottom_sheet.set_bottom_bar(Some(&bar_box));
            bottom_sheet.set_open(false);
            bottom_sheet.set_show_drag_handle(true);
            bottom_sheet.set_modal(false);
            bottom_sheet.set_full_width(true);
            // Hide the bottom bar initially — revealed when upload starts.
            bottom_sheet.set_reveal_bottom_bar(false);

            obj.set_child(Some(&bottom_sheet));

            self.list_box.set(list_box).unwrap();
            let _ = self.bottom_sheet.set(bottom_sheet);
            let _ = self.progress_label.set(progress_label);
            let _ = self.progress_bar.set(progress_bar);
            let _ = self.detail_label.set(detail_label);
            let _ = self.bar_label.set(bar_label);
        }
    }

    impl imp::MomentsSidebar {
        /// Create the "Albums" header row with a "+" button.
        ///
        /// Uses top margin for visual separation from the routes above —
        /// no hard separator lines, following the GNOME spacing convention.
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
            row.set_margin_top(12); // visual gap from top routes

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
    ///
    /// The callback receives the route `id` of the selected entry.
    /// Album rows emit `"album:{uuid}"` as their route id.
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

    /// Populate all album rows at startup. Clears any existing album rows first.
    pub fn set_albums(&self, albums: &[(String, String)]) {
        // Clear existing album rows.
        let imp = self.imp();
        let mut album_rows = imp.album_rows.borrow_mut();
        let list_box = imp.list_box.get().unwrap();
        for (_, row) in album_rows.drain() {
            list_box.remove(&row);
        }
        drop(album_rows);

        // Add each album.
        for (id, name) in albums {
            self.add_album(id, name);
        }
    }

    /// Add a single album row to the Albums section.
    ///
    /// Idempotent — if an album with this ID already exists, updates its name instead.
    pub fn add_album(&self, album_id: &str, name: &str) {
        // If already present, just update the name (handles sync re-delivering existing albums).
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

        // Insert before the bottom separator.
        list_box.insert(&list_row, bottom_sep.index());

        // Attach right-click context menu if callbacks are set.
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
            // If this row is currently selected, fall back to "photos".
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
    ///
    /// When `add_album` creates a row, it attaches a right-click gesture
    /// that shows a popover with Rename/Delete using these callbacks.
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

    // ── Upload progress bottom sheet ────────────────────────────────

    /// Show upload progress in the sidebar bottom sheet.
    pub fn show_upload_progress(&self, current: usize, total: usize) {
        let imp = self.imp();
        if let Some(sheet) = imp.bottom_sheet.get() {
            sheet.set_reveal_bottom_bar(true);
        }
        if let Some(label) = imp.bar_label.get() {
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
    }

    /// Show upload complete summary, then auto-hide after 3 seconds.
    pub fn show_upload_complete(&self, summary: &crate::library::import::ImportSummary) {
        let imp = self.imp();
        if let Some(label) = imp.bar_label.get() {
            label.set_text(&format!("{} imported", summary.imported));
        }
        if let Some(label) = imp.progress_label.get() {
            label.set_text("Upload Complete");
        }
        if let Some(bar) = imp.progress_bar.get() {
            bar.set_fraction(1.0);
        }
        let mut detail = format!("{} imported", summary.imported);
        if summary.skipped_duplicates > 0 {
            detail.push_str(&format!(", {} skipped", summary.skipped_duplicates));
        }
        if summary.failed > 0 {
            detail.push_str(&format!(", {} failed", summary.failed));
        }
        if let Some(label) = imp.detail_label.get() {
            label.set_text(&detail);
        }

        // Close sheet if open.
        if let Some(sheet) = imp.bottom_sheet.get() {
            sheet.set_open(false);
        }

        // Auto-hide the bottom bar after 3 seconds.
        let obj_weak = self.downgrade();
        glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
            if let Some(obj) = obj_weak.upgrade() {
                obj.hide_upload_progress();
            }
        });
    }

    /// Hide the upload progress bottom bar.
    pub fn hide_upload_progress(&self) {
        let imp = self.imp();
        if let Some(sheet) = imp.bottom_sheet.get() {
            sheet.set_reveal_bottom_bar(false);
            sheet.set_open(false);
        }
    }
}

pub mod route;
pub mod row;

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::{glib, prelude::*, subclass::prelude::*};
use adw::prelude::NavigationPageExt;
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
            obj.set_child(Some(&toolbar_view));

            self.list_box.set(list_box).unwrap();
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
}

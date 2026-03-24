pub mod route;
pub mod row;

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::{glib, prelude::*, subclass::prelude::*};
use gtk::gio;
use adw::prelude::NavigationPageExt;
use tracing::debug;

use route::{TOP_ROUTES, BOTTOM_ROUTES};
use row::MomentsSidebarRow;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default)]
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
    pub fn add_album(&self, album_id: &str, name: &str) {
        let imp = self.imp();
        let list_box = imp.list_box.get().unwrap();
        let bottom_sep = imp.bottom_separator.get().unwrap();

        let route_id = format!("album:{album_id}");
        let sidebar_row = MomentsSidebarRow::new(&route_id, name, "folder-symbolic");
        let list_row = gtk::ListBoxRow::new();
        list_row.set_child(Some(&sidebar_row));

        // Insert before the bottom separator.
        list_box.insert(&list_row, bottom_sep.index());

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

    /// Connect a right-click handler on album rows.
    ///
    /// When a user right-clicks an album row, a popover menu is shown with
    /// Rename and Delete actions. The callbacks receive the album_id.
    pub fn setup_album_context_menu(
        &self,
        menu_model: gio::MenuModel,
        on_rename: impl Fn(String, String) + 'static,
        on_delete: impl Fn(String, String) + 'static,
    ) {
        let list_box = self.imp().list_box.get().unwrap();

        // Right-click gesture on the ListBox.
        let gesture = gtk::GestureClick::new();
        gesture.set_button(3); // secondary (right) button

        let menu = menu_model;
        let on_rename = std::rc::Rc::new(on_rename);
        let on_delete = std::rc::Rc::new(on_delete);

        gesture.connect_pressed(glib::clone!(
            #[weak]
            list_box,
            move |gesture, _, x, y| {
                // Find which row was clicked.
                let Some(row) = list_box.row_at_y(y as i32) else { return };
                let Some(child) = row.child() else { return };
                let Some(sidebar_row) = child.downcast_ref::<MomentsSidebarRow>() else {
                    return;
                };

                let route_id = sidebar_row.route_id().to_owned();
                // Only handle album rows (route_id starts with "album:").
                let Some(album_id) = route_id.strip_prefix("album:") else { return };
                let album_id = album_id.to_owned();
                let album_name = sidebar_row.label_text().to_owned();

                // Create action group with rename/delete for this album.
                let action_group = gio::SimpleActionGroup::new();

                let rename_cb = on_rename.clone();
                let aid = album_id.clone();
                let aname = album_name.clone();
                let rename_action = gio::SimpleAction::new("rename", None);
                rename_action.connect_activate(move |_, _| {
                    rename_cb(aid.clone(), aname.clone());
                });
                action_group.add_action(&rename_action);

                let delete_cb = on_delete.clone();
                let aid = album_id.clone();
                let aname = album_name.clone();
                let delete_action = gio::SimpleAction::new("delete", None);
                delete_action.connect_activate(move |_, _| {
                    delete_cb(aid.clone(), aname.clone());
                });
                action_group.add_action(&delete_action);

                // Build and show popover menu.
                let popover = gtk::PopoverMenu::from_model(Some(&menu));
                popover.insert_action_group("album", Some(&action_group));
                popover.set_parent(&row);
                popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                    x as i32, 0, 1, 1,
                )));
                popover.set_has_arrow(true);

                // Clean up when popover is closed.
                popover.connect_closed(move |p| {
                    p.unparent();
                });

                popover.popup();
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        ));

        list_box.add_controller(gesture);
    }
}

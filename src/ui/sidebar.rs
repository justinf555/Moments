pub mod route;
pub mod row;

use gtk::{glib, prelude::*, subclass::prelude::*};
use adw::prelude::NavigationPageExt;
use tracing::debug;

use route::ROUTES;
use row::MomentsSidebarRow;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default)]
    pub struct MomentsSidebar {
        pub list_box: OnceCell<gtk::ListBox>,
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

            for route in ROUTES {
                let row = MomentsSidebarRow::new(route.id, route.label, route.icon);
                let list_row = gtk::ListBoxRow::new();
                list_row.set_child(Some(&row));
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
}

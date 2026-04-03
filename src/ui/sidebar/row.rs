use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default)]
    pub struct MomentsSidebarRow {
        pub route_id: OnceCell<String>,
        pub label: OnceCell<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsSidebarRow {
        const NAME: &'static str = "MomentsSidebarRow";
        type Type = super::MomentsSidebarRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("sidebar-row");
        }
    }

    impl ObjectImpl for MomentsSidebarRow {}
    impl WidgetImpl for MomentsSidebarRow {}
    impl BoxImpl for MomentsSidebarRow {}
}

glib::wrapper! {
    pub struct MomentsSidebarRow(ObjectSubclass<imp::MomentsSidebarRow>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl MomentsSidebarRow {
    /// Build a sidebar row for the given route.
    pub fn new(route_id: &str, label: &str, icon: &str) -> Self {
        let obj: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Horizontal)
            .property("spacing", 12)
            .build();

        obj.imp()
            .route_id
            .set(route_id.to_owned())
            .expect("route_id set once");

        let image = gtk::Image::from_icon_name(icon);
        obj.append(&image);

        let label_widget = gtk::Label::new(Some(label));
        label_widget.set_xalign(0.0);
        label_widget.set_hexpand(true);
        obj.append(&label_widget);

        obj.imp()
            .label
            .set(label_widget)
            .expect("label set once");

        // Let the parent ListBoxRow be the AT-SPI node; suppress this Box.
        obj.update_property(&[gtk::accessible::Property::Label(label)]);
        obj.set_accessible_role(gtk::AccessibleRole::None);

        obj
    }

    pub fn route_id(&self) -> &str {
        self.imp().route_id.get().map(|s| s.as_str()).unwrap_or("")
    }

    /// Update the displayed label text and accessible label.
    pub fn set_label_text(&self, text: &str) {
        if let Some(label) = self.imp().label.get() {
            label.set_text(text);
        }
        self.update_property(&[gtk::accessible::Property::Label(text)]);
    }

    /// Get the current label text.
    pub fn label_text(&self) -> String {
        self.imp()
            .label
            .get()
            .map(|l| l.text().to_string())
            .unwrap_or_default()
    }
}

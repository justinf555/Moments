use std::cell::RefCell;

use gettextrs::gettext;
use gtk::{glib, prelude::*, subclass::prelude::*};

use super::item::PersonItemObject;

/// Bindings held while a cell is bound to an item.
pub(super) struct CellBindings {
    item: glib::WeakRef<PersonItemObject>,
    texture_handler: glib::SignalHandlerId,
}

#[allow(private_interfaces)]
mod imp {
    use super::*;
    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/people_grid/cell.ui")]
    pub struct PeopleGridCell {
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub hidden_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub name_label: TemplateChild<gtk::Label>,

        pub bindings: RefCell<Option<CellBindings>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PeopleGridCell {
        const NAME: &'static str = "MomentsPeopleGridCell";
        type Type = super::PeopleGridCell;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BoxLayout>();
            klass.set_css_name("people-grid-cell");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PeopleGridCell {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            let layout = obj
                .layout_manager()
                .and_downcast::<gtk::BoxLayout>()
                .unwrap();
            layout.set_orientation(gtk::Orientation::Vertical);
            layout.set_spacing(4);
        }

        fn dispose(&self) {
            self.dispose_template();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PeopleGridCell {}
}

glib::wrapper! {
    pub struct PeopleGridCell(ObjectSubclass<imp::PeopleGridCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PeopleGridCell {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Bind the cell to a person item.
    pub fn bind(&self, item: &PersonItemObject) {
        let imp = self.imp();
        let data = item.data();

        let display_name = if data.name.is_empty() {
            "Unnamed"
        } else {
            &data.name
        };
        imp.name_label.set_text(display_name);

        // AdwAvatar uses the text to generate consistent colour + initials.
        imp.avatar.set_text(Some(display_name));

        // Set face thumbnail as custom image if available.
        if let Some(texture) = item.texture() {
            imp.avatar.set_custom_image(Some(&texture));
        }

        // Build accessible label.
        let a11y_label = if data.is_hidden {
            format!("{}, {}", display_name, gettext("hidden"))
        } else {
            display_name.to_string()
        };
        self.update_property(&[gtk::accessible::Property::Label(&a11y_label)]);

        if data.is_hidden {
            self.add_css_class("hidden-person");
            imp.hidden_icon.set_visible(true);
        } else {
            self.remove_css_class("hidden-person");
            imp.hidden_icon.set_visible(false);
        }

        // Watch for async thumbnail loads.
        let avatar = imp.avatar.clone();
        let texture_handler = item.connect_texture_notify(move |item| {
            if let Some(texture) = item.texture() {
                avatar.set_custom_image(Some(&texture));
            }
        });

        *imp.bindings.borrow_mut() = Some(CellBindings {
            item: item.downgrade(),
            texture_handler,
        });
    }

    /// Return the currently bound item, if any.
    pub fn bound_item(&self) -> Option<PersonItemObject> {
        self.imp().bindings.borrow().as_ref()?.item.upgrade()
    }

    /// Unbind the cell, disconnecting signals.
    pub fn unbind(&self) {
        let imp = self.imp();
        if let Some(b) = imp.bindings.borrow_mut().take() {
            if let Some(item) = b.item.upgrade() {
                item.disconnect(b.texture_handler);
            }
        }
        imp.avatar.set_custom_image(None::<&gtk::gdk::Paintable>);
        imp.avatar.set_text(None);
        imp.name_label.set_text("");
        imp.hidden_icon.set_visible(false);
        self.remove_css_class("hidden-person");
    }
}

impl Default for PeopleGridCell {
    fn default() -> Self {
        Self::new()
    }
}

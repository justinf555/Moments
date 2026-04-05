use std::cell::RefCell;

use gettextrs::gettext;
use gtk::{glib, prelude::*, subclass::prelude::*};

use super::item::CollectionItemObject;

/// Bindings held while a cell is bound to an item.
pub(super) struct CellBindings {
    item: glib::WeakRef<CollectionItemObject>,
    texture_handler: glib::SignalHandlerId,
}

#[allow(private_interfaces)]
mod imp {
    use super::*;
    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/collection_grid/cell.ui")]
    pub struct CollectionGridCell {
        #[template_child]
        pub picture: TemplateChild<gtk::Picture>,
        #[template_child]
        pub placeholder: TemplateChild<gtk::Image>,
        #[template_child]
        pub hidden_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,

        pub bindings: RefCell<Option<CellBindings>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionGridCell {
        const NAME: &'static str = "MomentsCollectionGridCell";
        type Type = super::CollectionGridCell;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BoxLayout>();
            klass.set_css_name("collection-grid-cell");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CollectionGridCell {
        fn constructed(&self) {
            self.parent_constructed();

            // Configure BoxLayout orientation and spacing.
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
        }
    }

    impl WidgetImpl for CollectionGridCell {}
}

glib::wrapper! {
    pub struct CollectionGridCell(ObjectSubclass<imp::CollectionGridCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CollectionGridCell {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Bind the cell to a collection item.
    pub fn bind(&self, item: &CollectionItemObject) {
        let imp = self.imp();
        let data = item.data();

        let display_name = if data.name.is_empty() {
            "Unnamed"
        } else {
            &data.name
        };
        imp.name_label.set_text(display_name);
        imp.subtitle_label.set_text(&data.subtitle);

        // Build accessible label: "name, subtitle" or "name, subtitle, hidden".
        let a11y_label = {
            let base = if data.subtitle.is_empty() {
                display_name.to_string()
            } else {
                format!("{}, {}", display_name, data.subtitle)
            };
            if data.is_hidden {
                // Translators: appended to person name for hidden people.
                format!("{}, {}", base, gettext("hidden"))
            } else {
                base
            }
        };
        self.update_property(&[gtk::accessible::Property::Label(&a11y_label)]);

        if data.is_hidden {
            self.add_css_class("hidden-person");
            imp.hidden_icon.set_visible(true);
        } else {
            self.remove_css_class("hidden-person");
            imp.hidden_icon.set_visible(false);
        }

        if let Some(texture) = item.texture() {
            imp.picture.set_paintable(Some(&texture));
            imp.picture.set_visible(true);
            imp.placeholder.set_visible(false);
        }

        let picture = imp.picture.clone();
        let placeholder = imp.placeholder.clone();
        let texture_handler = item.connect_texture_notify(move |item| {
            if let Some(texture) = item.texture() {
                picture.set_paintable(Some(&texture));
                picture.set_visible(true);
                placeholder.set_visible(false);
            }
        });

        *imp.bindings.borrow_mut() = Some(CellBindings {
            item: item.downgrade(),
            texture_handler,
        });
    }

    /// Return the currently bound item, if any.
    pub fn bound_item(&self) -> Option<CollectionItemObject> {
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
        imp.picture.set_paintable(None::<&gtk::gdk::Texture>);
        imp.picture.set_visible(false);
        imp.placeholder.set_visible(true);
        imp.name_label.set_text("");
        imp.subtitle_label.set_text("");
        imp.hidden_icon.set_visible(false);
        self.remove_css_class("hidden-person");
    }
}

impl Default for CollectionGridCell {
    fn default() -> Self {
        Self::new()
    }
}

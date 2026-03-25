use std::cell::RefCell;

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

    pub struct CollectionGridCell {
        pub picture: gtk::Picture,
        pub placeholder: gtk::Image,
        pub hidden_icon: gtk::Image,
        pub name_label: gtk::Label,
        pub subtitle_label: gtk::Label,
        pub bindings: RefCell<Option<CellBindings>>,
    }

    impl Default for CollectionGridCell {
        fn default() -> Self {
            Self {
                picture: gtk::Picture::new(),
                placeholder: gtk::Image::from_icon_name("avatar-default-symbolic"),
                hidden_icon: gtk::Image::from_icon_name("eye-not-looking-symbolic"),
                name_label: gtk::Label::new(None),
                subtitle_label: gtk::Label::new(None),
                bindings: RefCell::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionGridCell {
        const NAME: &'static str = "MomentsCollectionGridCell";
        type Type = super::CollectionGridCell;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BoxLayout>();
            klass.set_css_name("collection-grid-cell");
        }
    }

    impl ObjectImpl for CollectionGridCell {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Use BoxLayout vertical orientation.
            let layout = obj
                .layout_manager()
                .and_downcast::<gtk::BoxLayout>()
                .unwrap();
            layout.set_orientation(gtk::Orientation::Vertical);
            layout.set_spacing(4);

            // Thumbnail frame — a fixed-size box that clips its child.
            let frame = gtk::Frame::new(None);
            frame.set_halign(gtk::Align::Center);
            frame.set_size_request(96, 96);
            frame.set_overflow(gtk::Overflow::Hidden);
            frame.add_css_class("collection-thumbnail-frame");

            // Overlay: placeholder behind picture.
            let overlay = gtk::Overlay::new();

            self.placeholder.set_pixel_size(96);
            self.placeholder.add_css_class("dim-label");
            overlay.set_child(Some(&self.placeholder));

            self.picture.set_size_request(96, 96);
            self.picture.set_content_fit(gtk::ContentFit::Cover);
            self.picture.set_visible(false);
            overlay.add_overlay(&self.picture);

            // Hidden indicator icon — shown over the thumbnail when person is hidden.
            self.hidden_icon.set_pixel_size(32);
            self.hidden_icon.set_halign(gtk::Align::Center);
            self.hidden_icon.set_valign(gtk::Align::Center);
            self.hidden_icon.set_visible(false);
            self.hidden_icon.add_css_class("hidden-icon");
            overlay.add_overlay(&self.hidden_icon);

            frame.set_child(Some(&overlay));
            frame.set_parent(&*obj);

            // Name label.
            self.name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            self.name_label.set_max_width_chars(14);
            self.name_label.set_halign(gtk::Align::Center);
            self.name_label.add_css_class("caption-heading");
            self.name_label.set_parent(&*obj);

            // Subtitle label.
            self.subtitle_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            self.subtitle_label.set_max_width_chars(14);
            self.subtitle_label.set_halign(gtk::Align::Center);
            self.subtitle_label.add_css_class("dim-label");
            self.subtitle_label.add_css_class("caption");
            self.subtitle_label.set_parent(&*obj);
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
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

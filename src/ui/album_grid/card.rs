use std::cell::RefCell;

use gtk::{glib, prelude::*, subclass::prelude::*};

use super::item::AlbumItemObject;

/// Handler IDs stored between `bind` and `unbind` calls.
pub struct CardBindings {
    item: glib::WeakRef<AlbumItemObject>,
    texture_handler: glib::SignalHandlerId,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct AlbumCard {
        pub picture: gtk::Picture,
        pub placeholder: gtk::Image,
        pub name_label: gtk::Label,
        pub count_label: gtk::Label,
        pub bindings: RefCell<Option<CardBindings>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumCard {
        const NAME: &'static str = "MomentsAlbumCard";
        type Type = super::AlbumCard;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BoxLayout>();
            klass.set_css_name("album-card");
        }
    }

    impl ObjectImpl for AlbumCard {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let layout = obj
                .layout_manager()
                .and_downcast::<gtk::BoxLayout>()
                .unwrap();
            layout.set_orientation(gtk::Orientation::Vertical);
            layout.set_spacing(4);

            // Inner box — fixed width, centered in the cell. Frame and labels
            // are children of this box so their left edges align naturally.
            let inner = gtk::Box::new(gtk::Orientation::Vertical, 4);
            inner.set_size_request(155, -1);
            inner.set_halign(gtk::Align::Center);

            // Cover frame — clipped square with rounded corners.
            let frame = gtk::Frame::new(None);
            frame.set_size_request(155, 155);
            frame.set_overflow(gtk::Overflow::Hidden);
            frame.add_css_class("album-cover-frame");

            let overlay = gtk::Overlay::new();

            self.placeholder.set_pixel_size(48);
            self.placeholder.set_icon_name(Some("folder-symbolic"));
            self.placeholder.add_css_class("dim-label");
            self.placeholder.set_halign(gtk::Align::Center);
            self.placeholder.set_valign(gtk::Align::Center);
            overlay.set_child(Some(&self.placeholder));

            self.picture.set_size_request(155, 155);
            self.picture.set_content_fit(gtk::ContentFit::Cover);
            self.picture.set_visible(false);
            overlay.add_overlay(&self.picture);

            frame.set_child(Some(&overlay));
            inner.append(&frame);

            // Name label.
            self.name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            self.name_label.set_max_width_chars(18);
            self.name_label.set_halign(gtk::Align::Start);
            self.name_label.set_xalign(0.0);
            self.name_label.add_css_class("heading");
            inner.append(&self.name_label);

            // Count label.
            self.count_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            self.count_label.set_max_width_chars(18);
            self.count_label.set_halign(gtk::Align::Start);
            self.count_label.set_xalign(0.0);
            self.count_label.add_css_class("dim-label");
            self.count_label.add_css_class("caption");
            inner.append(&self.count_label);

            inner.set_parent(&*obj);
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for AlbumCard {}
}

glib::wrapper! {
    pub struct AlbumCard(ObjectSubclass<imp::AlbumCard>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AlbumCard {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Bind the card to an album item.
    pub fn bind(&self, item: &AlbumItemObject) {
        let imp = self.imp();
        let album = item.album();

        imp.name_label.set_text(&album.name);
        let count_text = if album.media_count == 1 {
            "1 photo".to_string()
        } else {
            format!("{} photos", album.media_count)
        };
        imp.count_label.set_text(&count_text);

        // Show texture if already decoded.
        if let Some(texture) = item.texture() {
            imp.picture.set_paintable(Some(&texture));
            imp.picture.set_visible(true);
            imp.placeholder.set_visible(false);
        }

        // Watch for texture changes (async decode completion).
        let picture = imp.picture.clone();
        let placeholder = imp.placeholder.clone();
        let texture_handler = item.connect_texture_notify(move |item| {
            if let Some(texture) = item.texture() {
                picture.set_paintable(Some(&texture));
                picture.set_visible(true);
                placeholder.set_visible(false);
            }
        });

        *imp.bindings.borrow_mut() = Some(CardBindings {
            item: item.downgrade(),
            texture_handler,
        });
    }

    /// Unbind the card, disconnecting signals.
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
        imp.count_label.set_text("");
    }
}

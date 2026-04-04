use std::cell::RefCell;

use gtk::{glib, prelude::*, subclass::prelude::*};

use super::item::AlbumItemObject;

/// Signal handler IDs stored between `bind` and `unbind`.
pub struct CardBindings {
    item: glib::WeakRef<AlbumItemObject>,
    texture_handlers: Vec<glib::SignalHandlerId>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct AlbumCard {
        /// 4 pictures for the 2×2 mosaic.
        pub pictures: [gtk::Picture; 4],
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
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("album-card");
        }
    }

    impl ObjectImpl for AlbumCard {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Inner box — fixed width, centered in the cell.
            let inner = gtk::Box::new(gtk::Orientation::Vertical, 4);
            inner.set_size_request(205, -1);
            inner.set_halign(gtk::Align::Center);

            // Cover frame — clipped square with rounded corners.
            let frame = gtk::Frame::new(None);
            frame.set_size_request(205, 205);
            frame.set_overflow(gtk::Overflow::Hidden);
            frame.add_css_class("album-cover-frame");

            let overlay = gtk::Overlay::new();

            // Placeholder (shown when no photos).
            self.placeholder.set_pixel_size(48);
            self.placeholder.set_icon_name(Some("folder-symbolic"));
            self.placeholder.add_css_class("dim-label");
            self.placeholder.set_halign(gtk::Align::Center);
            self.placeholder.set_valign(gtk::Align::Center);
            overlay.set_child(Some(&self.placeholder));

            // 2×2 mosaic grid.
            let grid = gtk::Grid::new();
            grid.set_row_spacing(2);
            grid.set_column_spacing(2);
            grid.set_row_homogeneous(true);
            grid.set_column_homogeneous(true);
            grid.set_visible(false);

            for (i, pic) in self.pictures.iter().enumerate() {
                pic.set_content_fit(gtk::ContentFit::Cover);
                pic.set_hexpand(true);
                pic.set_vexpand(true);
                grid.attach(pic, (i % 2) as i32, (i / 2) as i32, 1, 1);
            }

            overlay.add_overlay(&grid);

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

        // Apply any already-decoded mosaic textures.
        self.apply_mosaic_textures(item);

        // Watch for texture changes on all 4 slots.
        let mut handlers = Vec::new();
        let props = ["texture0", "texture1", "texture2", "texture3"];
        for prop in &props {
            let card = self.clone();
            let item_weak = item.downgrade();
            let handler = item.connect_notify_local(Some(prop), move |_, _| {
                if let Some(item) = item_weak.upgrade() {
                    card.apply_mosaic_textures(&item);
                }
            });
            handlers.push(handler);
        }

        *imp.bindings.borrow_mut() = Some(CardBindings {
            item: item.downgrade(),
            texture_handlers: handlers,
        });
    }

    /// Unbind the card, disconnecting signals.
    pub fn unbind(&self) {
        let imp = self.imp();
        if let Some(b) = imp.bindings.borrow_mut().take() {
            if let Some(item) = b.item.upgrade() {
                for handler in b.texture_handlers {
                    item.disconnect(handler);
                }
            }
        }
        for pic in &imp.pictures {
            pic.set_paintable(None::<&gtk::gdk::Texture>);
        }
        imp.name_label.set_text("");
        imp.count_label.set_text("");

        // Hide mosaic grid, show placeholder.
        self.set_mosaic_visible(false);
    }

    /// Apply mosaic textures from the item to the grid pictures.
    fn apply_mosaic_textures(&self, item: &AlbumItemObject) {
        let imp = self.imp();
        let mut any_set = false;

        for i in 0..4 {
            if let Some(texture) = item.mosaic_texture(i) {
                imp.pictures[i].set_paintable(Some(&texture));
                any_set = true;
            }
        }

        // If fewer than 4 textures, fill remaining slots by repeating.
        if any_set {
            let mut last_texture = None;
            for i in 0..4 {
                if let Some(t) = item.mosaic_texture(i) {
                    last_texture = Some(t);
                } else if let Some(ref t) = last_texture {
                    imp.pictures[i].set_paintable(Some(t));
                }
            }
            self.set_mosaic_visible(true);
        }
    }

    /// Show or hide the mosaic grid and placeholder.
    fn set_mosaic_visible(&self, visible: bool) {
        let imp = self.imp();
        imp.placeholder.set_visible(!visible);
        // The grid is the overlay child — find it.
        if let Some(parent) = imp.placeholder.parent() {
            if let Some(overlay) = parent.downcast_ref::<gtk::Overlay>() {
                // The grid is the first overlay widget.
                let mut child = overlay.first_child();
                while let Some(c) = child {
                    if c.downcast_ref::<gtk::Grid>().is_some() {
                        c.set_visible(visible);
                        break;
                    }
                    child = c.next_sibling();
                }
            }
        }
    }
}

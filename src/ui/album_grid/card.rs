use std::cell::{Cell, RefCell};

use gtk::{glib, prelude::*, subclass::prelude::*};

use super::item::AlbumItemObject;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayMode {
    Placeholder,
    Single,
    Mosaic,
}

/// Signal handler IDs stored between `bind` and `unbind`.
pub struct CardBindings {
    item: glib::WeakRef<AlbumItemObject>,
    texture_handlers: Vec<glib::SignalHandlerId>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct AlbumCard {
        /// Single cover picture (shown for 1–3 photos).
        pub single_picture: gtk::Picture,
        /// 4 pictures for the 2×2 mosaic (shown for 4+ photos).
        pub pictures: [gtk::Picture; 4],
        /// Mosaic grid widget.
        pub mosaic_grid: std::cell::OnceCell<gtk::Grid>,
        pub placeholder: gtk::Image,
        pub checkbox: gtk::CheckButton,
        pub name_label: gtk::Label,
        pub count_label: gtk::Label,
        pub in_selection_mode: Cell<bool>,
        /// Click handler for the checkbox — connected in factory bind.
        pub checkbox_handler: RefCell<Option<glib::SignalHandlerId>>,
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

            // Single cover picture (1–3 photos).
            self.single_picture.set_content_fit(gtk::ContentFit::Cover);
            self.single_picture.set_hexpand(true);
            self.single_picture.set_vexpand(true);
            self.single_picture.set_visible(false);
            overlay.add_overlay(&self.single_picture);

            // 2×2 mosaic grid (4+ photos).
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
            let _ = self.mosaic_grid.set(grid);

            // Selection checkbox — top-left, shown in selection mode.
            self.checkbox.set_halign(gtk::Align::Start);
            self.checkbox.set_valign(gtk::Align::Start);
            self.checkbox.set_margin_start(6);
            self.checkbox.set_margin_top(6);
            self.checkbox.add_css_class("selection-mode");
            self.checkbox.add_css_class("osd");
            self.checkbox.set_visible(false);
            overlay.add_overlay(&self.checkbox);

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

    /// Set whether the card is in selection mode (checkbox always visible).
    pub fn set_selection_mode(&self, active: bool) {
        let imp = self.imp();
        imp.in_selection_mode.set(active);
        imp.checkbox.set_visible(active);
        if !active {
            imp.checkbox.set_active(false);
        }
    }

    /// Set the checkbox checked state.
    pub fn set_checked(&self, checked: bool) {
        self.imp().checkbox.set_active(checked);
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
        imp.single_picture.set_paintable(None::<&gtk::gdk::Texture>);
        for pic in &imp.pictures {
            pic.set_paintable(None::<&gtk::gdk::Texture>);
        }
        imp.checkbox.set_visible(false);
        imp.checkbox.set_active(false);
        imp.name_label.set_text("");
        imp.count_label.set_text("");
        self.set_display_mode(DisplayMode::Placeholder);
    }

    /// Apply mosaic textures from the item to the card.
    ///
    /// Three display modes:
    /// - 0 textures → placeholder icon
    /// - 1–3 textures → single cover photo filling the frame
    /// - 4 textures → 2×2 mosaic grid
    fn apply_mosaic_textures(&self, item: &AlbumItemObject) {
        let imp = self.imp();

        // Count how many textures are available.
        let count = (0..4).filter(|i| item.mosaic_texture(*i).is_some()).count();

        if count == 0 {
            self.set_display_mode(DisplayMode::Placeholder);
            return;
        }

        if count < 4 {
            // Single cover: use the first texture.
            if let Some(texture) = item.mosaic_texture(0) {
                imp.single_picture.set_paintable(Some(&texture));
            }
            self.set_display_mode(DisplayMode::Single);
        } else {
            // Full mosaic: set all 4 pictures.
            for i in 0..4 {
                if let Some(texture) = item.mosaic_texture(i) {
                    imp.pictures[i].set_paintable(Some(&texture));
                }
            }
            self.set_display_mode(DisplayMode::Mosaic);
        }
    }

    /// Switch between placeholder, single cover, and mosaic display.
    fn set_display_mode(&self, mode: DisplayMode) {
        let imp = self.imp();
        imp.placeholder.set_visible(mode == DisplayMode::Placeholder);
        imp.single_picture.set_visible(mode == DisplayMode::Single);
        if let Some(grid) = imp.mosaic_grid.get() {
            grid.set_visible(mode == DisplayMode::Mosaic);
        }
    }
}

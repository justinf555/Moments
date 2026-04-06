use std::cell::{Cell, RefCell};

use gettextrs::ngettext;
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
    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/album_grid/card.ui")]
    pub struct AlbumCard {
        #[template_child]
        pub single_picture: TemplateChild<gtk::Picture>,
        #[template_child]
        pub mosaic_grid: TemplateChild<gtk::Grid>,
        #[template_child]
        pub pic0: TemplateChild<gtk::Picture>,
        #[template_child]
        pub pic1: TemplateChild<gtk::Picture>,
        #[template_child]
        pub pic2: TemplateChild<gtk::Picture>,
        #[template_child]
        pub pic3: TemplateChild<gtk::Picture>,
        #[template_child]
        pub placeholder: TemplateChild<gtk::Image>,
        #[template_child]
        pub checkbox: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub count_label: TemplateChild<gtk::Label>,

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
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("album-card");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for AlbumCard {
        fn constructed(&self) {
            self.parent_constructed();

            // Hover controller — show checkbox on mouse enter/leave.
            let obj = self.obj();
            let motion = gtk::EventControllerMotion::new();
            motion.set_propagation_phase(gtk::PropagationPhase::Capture);
            let cell_weak = obj.downgrade();
            motion.connect_enter(move |_, _x, _y| {
                let Some(cell) = cell_weak.upgrade() else {
                    return;
                };
                cell.imp().checkbox.set_visible(true);
            });
            let cell_weak = obj.downgrade();
            motion.connect_leave(move |_| {
                let Some(cell) = cell_weak.upgrade() else {
                    return;
                };
                if !cell.imp().in_selection_mode.get() {
                    cell.imp().checkbox.set_visible(false);
                }
            });
            obj.add_controller(motion);
        }

        fn dispose(&self) {
            self.dispose_template();
            if let Some(child) = self.obj().first_child() {
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

impl Default for AlbumCard {
    fn default() -> Self {
        Self::new()
    }
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
        let count_text = ngettext("{} photo", "{} photos", album.media_count)
            .replace("{}", &album.media_count.to_string());
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

    /// Return the currently bound item, if any.
    pub fn bound_item(&self) -> Option<AlbumItemObject> {
        self.imp().bindings.borrow().as_ref()?.item.upgrade()
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
        let pictures = [&*imp.pic0, &*imp.pic1, &*imp.pic2, &*imp.pic3];
        for pic in &pictures {
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
            let pictures = [&*imp.pic0, &*imp.pic1, &*imp.pic2, &*imp.pic3];
            for (i, pic) in pictures.iter().enumerate() {
                if let Some(texture) = item.mosaic_texture(i) {
                    pic.set_paintable(Some(&texture));
                }
            }
            self.set_display_mode(DisplayMode::Mosaic);
        }
    }

    /// Switch between placeholder, single cover, and mosaic display.
    fn set_display_mode(&self, mode: DisplayMode) {
        let imp = self.imp();
        imp.placeholder
            .set_visible(mode == DisplayMode::Placeholder);
        imp.single_picture.set_visible(mode == DisplayMode::Single);
        imp.mosaic_grid.set_visible(mode == DisplayMode::Mosaic);
    }
}

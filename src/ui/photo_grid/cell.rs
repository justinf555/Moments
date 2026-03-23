use std::cell::RefCell;

use gtk::{glib, prelude::*, subclass::prelude::*};

use super::item::MediaItemObject;

/// Handler IDs stored between `bind` and `unbind` calls.
///
/// Typed struct instead of unsafe `widget.set_data()` / `steal_data()`.
/// Disconnected explicitly in `unbind` so no signals fire on stale items.
pub struct CellBindings {
    item: glib::WeakRef<MediaItemObject>,
    texture_handler: glib::SignalHandlerId,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PhotoGridCell {
        pub picture: gtk::Picture,
        pub spinner: gtk::Spinner,
        pub overlay: gtk::Overlay,
        pub bindings: RefCell<Option<CellBindings>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PhotoGridCell {
        const NAME: &'static str = "MomentsPhotoGridCell";
        type Type = super::PhotoGridCell;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("photo-grid-cell");
        }
    }

    impl ObjectImpl for PhotoGridCell {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            // Default cell size — overridden by the factory based on zoom level.
            obj.set_size_request(160, 160);

            self.picture.set_content_fit(gtk::ContentFit::Cover);
            self.picture.set_can_shrink(true);
            self.picture.set_visible(false);

            self.spinner.set_spinning(true);

            self.overlay.set_child(Some(&self.picture));
            self.overlay.add_overlay(&self.spinner);
            self.overlay.set_parent(&*obj);
        }

        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PhotoGridCell {}
}

glib::wrapper! {
    pub struct PhotoGridCell(ObjectSubclass<imp::PhotoGridCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PhotoGridCell {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Connect to `item` and reflect its current texture state.
    pub fn bind(&self, item: &MediaItemObject) {
        self.update_from_item(item);

        let cell = self.clone();
        let handler = item.connect_texture_notify(move |item| {
            cell.update_from_item(item);
        });

        *self.imp().bindings.borrow_mut() = Some(CellBindings {
            item: item.downgrade(),
            texture_handler: handler,
        });
    }

    /// Disconnect signals and reset visual state.
    pub fn unbind(&self) {
        let imp = self.imp();
        if let Some(b) = imp.bindings.borrow_mut().take() {
            if let Some(item) = b.item.upgrade() {
                item.disconnect(b.texture_handler);
            }
        }
        imp.picture.set_paintable(None::<&gtk::gdk::Texture>);
        imp.picture.set_visible(false);
        imp.spinner.set_spinning(true);
        imp.spinner.set_visible(true);
    }

    fn update_from_item(&self, item: &MediaItemObject) {
        let imp = self.imp();
        if let Some(texture) = item.texture() {
            imp.picture.set_paintable(Some(&texture));
            imp.picture.set_visible(true);
            imp.spinner.set_visible(false);
            imp.spinner.set_spinning(false);
        } else {
            imp.picture.set_paintable(None::<&gtk::gdk::Texture>);
            imp.picture.set_visible(false);
            imp.spinner.set_visible(true);
            imp.spinner.set_spinning(true);
        }
    }
}

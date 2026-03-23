use std::cell::{OnceCell, RefCell};

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

use crate::library::media::MediaItem;

mod imp {
    use super::*;
    use glib::Properties;

    /// GObject wrapper around a [`MediaItem`].
    ///
    /// The `texture` property starts `None` and is set once the thumbnail is
    /// ready on disk. Cells bind to `notify::texture` to repaint without polling.
    #[derive(Default, Properties)]
    #[properties(wrapper_type = super::MediaItemObject)]
    pub struct MediaItemObject {
        /// The underlying media item — set once at construction, never mutated.
        pub item: OnceCell<MediaItem>,

        /// The decoded thumbnail texture, `None` until the thumbnail is ready.
        #[property(get, set, nullable)]
        pub texture: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MediaItemObject {
        const NAME: &'static str = "MomentsMediaItemObject";
        type Type = super::MediaItemObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for MediaItemObject {}
}

glib::wrapper! {
    pub struct MediaItemObject(ObjectSubclass<imp::MediaItemObject>);
}

impl MediaItemObject {
    pub fn new(item: MediaItem) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().item.set(item).expect("item set once at construction");
        obj
    }

    pub fn item(&self) -> &MediaItem {
        self.imp().item.get().expect("item initialised")
    }
}

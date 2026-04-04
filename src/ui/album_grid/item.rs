use std::cell::{OnceCell, RefCell};

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

use crate::library::album::Album;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::AlbumItemObject)]
    pub struct AlbumItemObject {
        pub album: OnceCell<Album>,

        /// The decoded cover thumbnail texture, `None` until ready.
        #[property(get, set, nullable)]
        pub texture: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumItemObject {
        const NAME: &'static str = "MomentsAlbumItemObject";
        type Type = super::AlbumItemObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for AlbumItemObject {}
}

glib::wrapper! {
    pub struct AlbumItemObject(ObjectSubclass<imp::AlbumItemObject>);
}

impl AlbumItemObject {
    pub fn new(album: Album) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().album.set(album).expect("album set once");
        obj
    }

    pub fn album(&self) -> &Album {
        self.imp().album.get().expect("album initialised")
    }
}

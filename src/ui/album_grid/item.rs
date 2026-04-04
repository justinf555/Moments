use std::cell::{OnceCell, RefCell};

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

use crate::library::album::Album;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::AlbumItemObject)]
    pub struct AlbumItemObject {
        pub album: OnceCell<Album>,

        /// Cover textures for the 2×2 mosaic (up to 4).
        #[property(get, set, nullable)]
        pub texture0: RefCell<Option<gdk::Texture>>,
        #[property(get, set, nullable)]
        pub texture1: RefCell<Option<gdk::Texture>>,
        #[property(get, set, nullable)]
        pub texture2: RefCell<Option<gdk::Texture>>,
        #[property(get, set, nullable)]
        pub texture3: RefCell<Option<gdk::Texture>>,
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

    /// Set a mosaic texture by index (0–3).
    pub fn set_mosaic_texture(&self, index: usize, texture: gdk::Texture) {
        match index {
            0 => self.set_texture0(Some(&texture)),
            1 => self.set_texture1(Some(&texture)),
            2 => self.set_texture2(Some(&texture)),
            3 => self.set_texture3(Some(&texture)),
            _ => {}
        }
    }

    /// Get a mosaic texture by index (0–3).
    pub fn mosaic_texture(&self, index: usize) -> Option<gdk::Texture> {
        match index {
            0 => self.texture0(),
            1 => self.texture1(),
            2 => self.texture2(),
            3 => self.texture3(),
            _ => None,
        }
    }
}

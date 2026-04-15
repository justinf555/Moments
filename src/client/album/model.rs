use std::cell::{Cell, RefCell};

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

use crate::library::album::Album;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::AlbumItemObject)]
    pub struct AlbumItemObject {
        #[property(get, set)]
        pub id: RefCell<String>,
        #[property(get, set)]
        pub name: RefCell<String>,
        #[property(get, set)]
        pub media_count: Cell<u32>,
        #[property(get, set)]
        pub created_at: Cell<i64>,
        #[property(get, set)]
        pub updated_at: Cell<i64>,
        #[property(get, set, nullable)]
        pub cover_media_id: RefCell<Option<String>>,
        #[property(get, set)]
        pub pinned: Cell<bool>,

        /// Cover textures for the 2x2 mosaic (up to 4).
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
        let imp = obj.imp();
        imp.id.replace(album.id.as_str().to_owned());
        imp.name.replace(album.name);
        imp.media_count.set(album.media_count);
        imp.created_at.set(album.created_at);
        imp.updated_at.set(album.updated_at);
        imp.cover_media_id
            .replace(album.cover_media_id.map(|mid| mid.as_str().to_owned()));
        imp.pinned.set(album.is_pinned);
        obj
    }

    /// Set a mosaic texture by index (0-3).
    pub fn set_mosaic_texture(&self, index: usize, texture: gdk::Texture) {
        match index {
            0 => self.set_texture0(Some(&texture)),
            1 => self.set_texture1(Some(&texture)),
            2 => self.set_texture2(Some(&texture)),
            3 => self.set_texture3(Some(&texture)),
            _ => {}
        }
    }

    /// Clear a mosaic texture by index (0-3).
    pub fn set_mosaic_texture_none(&self, index: usize) {
        match index {
            0 => self.set_texture0(None::<&gdk::Texture>),
            1 => self.set_texture1(None::<&gdk::Texture>),
            2 => self.set_texture2(None::<&gdk::Texture>),
            3 => self.set_texture3(None::<&gdk::Texture>),
            _ => {}
        }
    }

    /// Get a mosaic texture by index (0-3).
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

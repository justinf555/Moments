use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

/// Data for a single person in the people grid.
#[derive(Debug, Clone)]
pub struct PersonItemData {
    pub id: String,
    pub name: String,
    pub thumbnail_path: Option<PathBuf>,
    pub is_hidden: bool,
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::PersonItemObject)]
    pub struct PersonItemObject {
        pub data: OnceCell<PersonItemData>,

        /// The decoded thumbnail texture, `None` until ready.
        #[property(get, set, nullable)]
        pub texture: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PersonItemObject {
        const NAME: &'static str = "MomentsPersonItemObject";
        type Type = super::PersonItemObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for PersonItemObject {}
}

glib::wrapper! {
    pub struct PersonItemObject(ObjectSubclass<imp::PersonItemObject>);
}

impl PersonItemObject {
    pub fn new(data: PersonItemData) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().data.set(data).expect("data set once");
        obj
    }

    pub fn data(&self) -> &PersonItemData {
        self.imp().data.get().expect("data initialised")
    }
}

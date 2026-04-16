use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

use crate::library::faces::Person;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::PersonItemObject)]
    pub struct PersonItemObject {
        #[property(get, set)]
        pub id: RefCell<String>,
        #[property(get, set)]
        pub name: RefCell<String>,
        #[property(get, set)]
        pub is_hidden: Cell<bool>,

        /// File path to the person's face thumbnail (if available).
        pub thumbnail_path: RefCell<Option<PathBuf>>,

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
    pub fn new(person: &Person, thumbnail_path: Option<PathBuf>) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.id.replace(person.id.as_str().to_string());
        imp.name.replace(person.name.clone());
        imp.is_hidden.set(person.is_hidden);
        imp.thumbnail_path.replace(thumbnail_path);
        obj
    }

    /// The file path to this person's face thumbnail.
    pub fn thumbnail_path(&self) -> Option<PathBuf> {
        self.imp().thumbnail_path.borrow().clone()
    }
}

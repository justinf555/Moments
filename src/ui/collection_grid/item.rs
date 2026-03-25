use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;

use gtk::{gdk, glib, prelude::*, subclass::prelude::*};

/// Data for a single collection grid item (person, memory, etc.).
#[derive(Debug, Clone)]
pub struct CollectionItemData {
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub thumbnail_path: Option<PathBuf>,
    pub is_hidden: bool,
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::CollectionItemObject)]
    pub struct CollectionItemObject {
        pub data: OnceCell<CollectionItemData>,

        /// The decoded thumbnail texture, `None` until ready.
        #[property(get, set, nullable)]
        pub texture: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionItemObject {
        const NAME: &'static str = "MomentsCollectionItemObject";
        type Type = super::CollectionItemObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for CollectionItemObject {}
}

glib::wrapper! {
    pub struct CollectionItemObject(ObjectSubclass<imp::CollectionItemObject>);
}

impl CollectionItemObject {
    pub fn new(data: CollectionItemData) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().data.set(data).expect("data set once");
        obj
    }

    pub fn data(&self) -> &CollectionItemData {
        self.imp().data.get().expect("data initialised")
    }
}

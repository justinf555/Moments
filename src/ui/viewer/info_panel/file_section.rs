use adw::subclass::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use crate::library::media::{MediaItem, MediaMetadataRecord};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/info_panel/file_section.ui")]
    pub struct InfoFileSection {
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub filename_value: TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InfoFileSection {
        const NAME: &'static str = "MomentsInfoFileSection";
        type Type = super::InfoFileSection;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for InfoFileSection {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for InfoFileSection {}
}

glib::wrapper! {
    pub struct InfoFileSection(ObjectSubclass<imp::InfoFileSection>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl InfoFileSection {
    pub fn set_item(&self, item: &MediaItem, _metadata: Option<&MediaMetadataRecord>) {
        let imp = self.imp();
        imp.subtitle_label.set_label(&item.original_filename);
        imp.filename_value.set_label(&item.original_filename);
    }
}

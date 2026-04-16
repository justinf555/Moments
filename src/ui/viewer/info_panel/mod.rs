mod camera_section;
mod date_section;
mod file_section;
mod image_section;
mod location_section;

use camera_section::InfoCameraSection;
use date_section::InfoDateSection;
use file_section::InfoFileSection;
use image_section::InfoImageSection;
use location_section::InfoLocationSection;

use adw::subclass::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use crate::library::media::MediaItem;
use crate::library::metadata::MediaMetadataRecord;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/info_panel/info_panel.ui")]
    pub struct InfoPanel {
        #[template_child]
        pub date_section: TemplateChild<InfoDateSection>,
        #[template_child]
        pub image_section: TemplateChild<InfoImageSection>,
        #[template_child]
        pub camera_section: TemplateChild<InfoCameraSection>,
        #[template_child]
        pub location_section: TemplateChild<InfoLocationSection>,
        #[template_child]
        pub file_section: TemplateChild<InfoFileSection>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InfoPanel {
        const NAME: &'static str = "MomentsInfoPanel";
        type Type = super::InfoPanel;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            // Ensure section types are registered before the panel template
            // references them.
            InfoDateSection::ensure_type();
            InfoImageSection::ensure_type();
            InfoCameraSection::ensure_type();
            InfoLocationSection::ensure_type();
            InfoFileSection::ensure_type();

            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for InfoPanel {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for InfoPanel {}
}

glib::wrapper! {
    pub struct InfoPanel(ObjectSubclass<imp::InfoPanel>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl InfoPanel {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Update all sections with the current item's data.
    ///
    /// Each section updates its labels in-place and manages its own
    /// visibility. The widget tree is never torn down — only labels change.
    pub fn set_item(&self, item: &MediaItem, metadata: Option<&MediaMetadataRecord>) {
        let imp = self.imp();
        imp.date_section.set_item(item, metadata);
        imp.image_section.set_item(item, metadata);
        imp.camera_section.set_item(item, metadata);
        imp.location_section.set_item(item, metadata);
        imp.file_section.set_item(item, metadata);
    }
}

impl Default for InfoPanel {
    fn default() -> Self {
        Self::new()
    }
}

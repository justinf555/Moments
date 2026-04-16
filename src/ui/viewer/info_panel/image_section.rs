use adw::subclass::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use crate::library::media::MediaItem;
use crate::library::metadata::MediaMetadataRecord;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/info_panel/image_section.ui")]
    pub struct InfoImageSection {
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub dimensions_row: TemplateChild<gtk::ListBoxRow>,
        #[template_child]
        pub dimensions_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub resolution_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub format_value: TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InfoImageSection {
        const NAME: &'static str = "MomentsInfoImageSection";
        type Type = super::InfoImageSection;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for InfoImageSection {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for InfoImageSection {}
}

glib::wrapper! {
    pub struct InfoImageSection(ObjectSubclass<imp::InfoImageSection>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl InfoImageSection {
    pub fn set_item(&self, item: &MediaItem, _metadata: Option<&MediaMetadataRecord>) {
        let imp = self.imp();

        let mp_str = match (item.width, item.height) {
            (Some(w), Some(h)) => {
                let mp = (w * h) as f64 / 1_000_000.0;
                format!("{mp:.1} MP")
            }
            _ => "Unknown".to_string(),
        };

        imp.subtitle_label.set_label(&mp_str);

        if let (Some(w), Some(h)) = (item.width, item.height) {
            imp.dimensions_value.set_label(&format!("{w} \u{d7} {h}"));
            imp.dimensions_row.set_visible(true);
        } else {
            imp.dimensions_row.set_visible(false);
        }

        imp.resolution_value.set_label(&mp_str);

        let format_str = item
            .original_filename
            .rsplit('.')
            .next()
            .map(|ext| ext.to_uppercase())
            .unwrap_or_else(|| "Unknown".to_string());
        imp.format_value.set_label(&format_str);
    }
}

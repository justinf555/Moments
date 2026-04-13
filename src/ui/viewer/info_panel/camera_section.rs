use adw::subclass::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use crate::library::media::{MediaItem, MediaMetadataRecord};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/info_panel/camera_section.ui")]
    pub struct InfoCameraSection {
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub camera_row: TemplateChild<gtk::ListBoxRow>,
        #[template_child]
        pub camera_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub lens_row: TemplateChild<gtk::ListBoxRow>,
        #[template_child]
        pub lens_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub exif_row: TemplateChild<gtk::ListBoxRow>,
        #[template_child]
        pub aperture_card: TemplateChild<gtk::Box>,
        #[template_child]
        pub aperture_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub shutter_card: TemplateChild<gtk::Box>,
        #[template_child]
        pub shutter_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub iso_card: TemplateChild<gtk::Box>,
        #[template_child]
        pub iso_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub focal_card: TemplateChild<gtk::Box>,
        #[template_child]
        pub focal_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub no_data_row: TemplateChild<gtk::ListBoxRow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InfoCameraSection {
        const NAME: &'static str = "MomentsInfoCameraSection";
        type Type = super::InfoCameraSection;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for InfoCameraSection {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for InfoCameraSection {}
}

glib::wrapper! {
    pub struct InfoCameraSection(ObjectSubclass<imp::InfoCameraSection>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl InfoCameraSection {
    pub fn set_item(&self, _item: &MediaItem, metadata: Option<&MediaMetadataRecord>) {
        let imp = self.imp();

        let camera_name = metadata.and_then(|m| match (&m.camera_make, &m.camera_model) {
            (Some(make), Some(model)) => {
                if model.starts_with(make.as_str()) {
                    Some(model.clone())
                } else {
                    Some(format!("{make} {model}"))
                }
            }
            (Some(make), None) => Some(make.clone()),
            (None, Some(model)) => Some(model.clone()),
            _ => None,
        });

        imp.subtitle_label
            .set_label(camera_name.as_deref().unwrap_or("No data"));

        // Camera row
        if let Some(ref name) = camera_name {
            imp.camera_value.set_label(name);
            imp.camera_row.set_visible(true);
        } else {
            imp.camera_row.set_visible(false);
        }

        // Lens row
        if let Some(lens) = metadata.and_then(|m| m.lens_model.as_deref()) {
            let lens_with_fl = metadata
                .and_then(|m| m.focal_length)
                .map(|fl| format!("{lens} \u{b7} {fl:.0}mm"))
                .unwrap_or_else(|| lens.to_string());
            imp.lens_value.set_label(&lens_with_fl);
            imp.lens_row.set_visible(true);
        } else {
            imp.lens_row.set_visible(false);
        }

        // EXIF cards — static widgets, just update labels and visibility
        let meta = metadata;
        let has_exif = meta
            .map(|m| {
                m.aperture.is_some()
                    || m.shutter_str.is_some()
                    || m.iso.is_some()
                    || m.focal_length.is_some()
            })
            .unwrap_or(false);

        if has_exif {
            let m = meta.unwrap();

            if let Some(f) = m.aperture {
                imp.aperture_value.set_label(&format!("f/{f:.1}"));
                imp.aperture_card.set_visible(true);
            } else {
                imp.aperture_card.set_visible(false);
            }

            if let Some(s) = &m.shutter_str {
                imp.shutter_value.set_label(&format!("{s}s"));
                imp.shutter_card.set_visible(true);
            } else {
                imp.shutter_card.set_visible(false);
            }

            if let Some(iso) = m.iso {
                imp.iso_value.set_label(&format!("{iso}"));
                imp.iso_card.set_visible(true);
            } else {
                imp.iso_card.set_visible(false);
            }

            if let Some(fl) = m.focal_length {
                imp.focal_value.set_label(&format!("{fl:.0}mm"));
                imp.focal_card.set_visible(true);
            } else {
                imp.focal_card.set_visible(false);
            }

            imp.exif_row.set_visible(true);
        } else {
            imp.exif_row.set_visible(false);
        }

        // No data placeholder
        let has_any_data = camera_name.is_some() || metadata.map(|m| m.has_data()).unwrap_or(false);
        imp.no_data_row.set_visible(!has_any_data);
    }
}

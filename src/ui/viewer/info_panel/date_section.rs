use adw::subclass::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use crate::library::media::{MediaItem, MediaMetadataRecord};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/info_panel/date_section.ui")]
    pub struct InfoDateSection {
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub captured_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub time_value: TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InfoDateSection {
        const NAME: &'static str = "MomentsInfoDateSection";
        type Type = super::InfoDateSection;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for InfoDateSection {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for InfoDateSection {}
}

glib::wrapper! {
    pub struct InfoDateSection(ObjectSubclass<imp::InfoDateSection>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl InfoDateSection {
    pub fn set_item(&self, item: &MediaItem, _metadata: Option<&MediaMetadataRecord>) {
        let imp = self.imp();
        let (short, long, time) = format_date_parts(item.taken_at);
        imp.subtitle_label.set_label(&short);
        imp.captured_value.set_label(&long);
        imp.time_value.set_label(&time);
    }
}

/// Split a Unix timestamp into (short date, long date, time) strings.
fn format_date_parts(ts: Option<i64>) -> (String, String, String) {
    use chrono::{DateTime, Utc};

    let Some(ts) = ts else {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into());
    };
    let Some(dt) = DateTime::<Utc>::from_timestamp(ts, 0) else {
        return ("Unknown".into(), "Unknown".into(), "Unknown".into());
    };

    let short = dt.format("%-d %b %Y").to_string();
    let long = dt.format("%-d %B %Y").to_string();
    let time = dt.format("%H:%M").to_string();

    (short, long, time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_date_parts_known_value() {
        let (short, long, time) = format_date_parts(Some(1_490_529_600));
        assert!(short.contains("2017"));
        assert!(long.contains("March"));
        assert!(time.contains("12:00"));
    }

    #[test]
    fn format_date_parts_none_returns_unknown() {
        let (short, long, time) = format_date_parts(None);
        assert_eq!(short, "Unknown");
        assert_eq!(long, "Unknown");
        assert_eq!(time, "Unknown");
    }
}

// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::subclass::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use crate::library::media::MediaItem;
use crate::library::metadata::MediaMetadataRecord;

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

/// Split a capture timestamp into (short date, long date, time) strings.
///
/// `taken_at` holds the *capture-local wall clock* — the time the camera's
/// own clock read — encoded as seconds since the epoch. Both import paths
/// agree on that: the EXIF extractor keeps `DateTimeOriginal` verbatim
/// (`library::metadata::exif`), and the Immich sync handler prefers the
/// server's `localDateTime`. Rendering with a fixed UTC offset therefore
/// hands back exactly those digits, which is what the panel should show.
///
/// Converting to the *viewer's* local timezone would be wrong: it would
/// slide a sunset shot taken at 18:04 in Sydney to "08:04" as soon as the
/// laptop landed in London, and it would shift every photo in the library
/// by the host offset even though no photo moved. Issue #549.
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
    fn format_date_parts_is_independent_of_host_timezone() {
        // A capture stored as 2024-06-15 18:04 wall clock must render as
        // 18:04 regardless of the machine's TZ. Issue #549.
        let ts = chrono::NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(18, 4, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let (short, long, time) = format_date_parts(Some(ts));
        assert_eq!(short, "15 Jun 2024");
        assert_eq!(long, "15 June 2024");
        assert_eq!(time, "18:04");
    }

    #[test]
    fn format_date_parts_out_of_range_returns_unknown() {
        let (short, long, time) = format_date_parts(Some(i64::MAX));
        assert_eq!(short, "Unknown");
        assert_eq!(long, "Unknown");
        assert_eq!(time, "Unknown");
    }

    #[test]
    fn format_date_parts_none_returns_unknown() {
        let (short, long, time) = format_date_parts(None);
        assert_eq!(short, "Unknown");
        assert_eq!(long, "Unknown");
        assert_eq!(time, "Unknown");
    }
}

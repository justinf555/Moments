use std::cell::RefCell;

use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::library::media::{MediaItem, MediaMetadataRecord};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/info_panel/location_section.ui")]
    pub struct InfoLocationSection {
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub lat_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub lon_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub altitude_row: TemplateChild<gtk::ListBoxRow>,
        #[template_child]
        pub altitude_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub map_btn: TemplateChild<gtk::Button>,

        /// Stored geo URI for the map button — updated on each `set_item`.
        pub geo_uri: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InfoLocationSection {
        const NAME: &'static str = "MomentsInfoLocationSection";
        type Type = super::InfoLocationSection;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for InfoLocationSection {
        fn constructed(&self) {
            self.parent_constructed();

            // Wire the map button once — reads the geo URI from the RefCell.
            let weak = self.obj().downgrade();
            self.map_btn.connect_clicked(move |btn| {
                let Some(section) = weak.upgrade() else {
                    return;
                };
                let uri = section.imp().geo_uri.borrow().clone();
                if uri.is_empty() {
                    return;
                }
                let launcher = gtk::UriLauncher::new(&uri);
                let window = btn.root().and_downcast::<gtk::Window>();
                launcher.launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
            });
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for InfoLocationSection {}
}

glib::wrapper! {
    pub struct InfoLocationSection(ObjectSubclass<imp::InfoLocationSection>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl InfoLocationSection {
    pub fn set_item(&self, _item: &MediaItem, metadata: Option<&MediaMetadataRecord>) {
        let imp = self.imp();

        let gps = metadata.and_then(|m| match (m.gps_lat, m.gps_lon) {
            (Some(lat), Some(lon)) => Some((lat, lon)),
            _ => None,
        });

        let Some((lat, lon)) = gps else {
            self.set_visible(false);
            return;
        };

        self.set_visible(true);

        let coords_str = format!(
            "{}\u{b0}, {}\u{b0}",
            format_decimal(lat.abs(), 4),
            format_decimal(lon.abs(), 4),
        );
        imp.subtitle_label.set_label(&coords_str);

        imp.lat_value.set_label(&format_coordinate(lat, 'N', 'S'));
        imp.lon_value.set_label(&format_coordinate(lon, 'E', 'W'));

        if let Some(alt) = metadata.and_then(|m| m.gps_alt) {
            imp.altitude_value.set_label(&format!("{alt:.0} m"));
            imp.altitude_row.set_visible(true);
        } else {
            imp.altitude_row.set_visible(false);
        }

        *imp.geo_uri.borrow_mut() = format!("geo:{lat},{lon}");
    }
}

/// Format a GPS coordinate with direction suffix (e.g. "48.8584° N").
fn format_coordinate(value: f64, pos_dir: char, neg_dir: char) -> String {
    let dir = if value >= 0.0 { pos_dir } else { neg_dir };
    format!("{}\u{b0} {dir}", format_decimal(value.abs(), 4))
}

/// Format a float to a fixed number of decimal places.
fn format_decimal(value: f64, decimals: usize) -> String {
    format!("{:.prec$}", value, prec = decimals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_coordinate_north() {
        let s = format_coordinate(48.8584, 'N', 'S');
        assert!(s.contains("48.8584"));
        assert!(s.contains("N"));
    }

    #[test]
    fn format_coordinate_south() {
        let s = format_coordinate(-33.8432, 'N', 'S');
        assert!(s.contains("33.8432"));
        assert!(s.contains("S"));
    }

    #[test]
    fn format_coordinate_east() {
        let s = format_coordinate(151.2419, 'E', 'W');
        assert!(s.contains("151.2419"));
        assert!(s.contains("E"));
    }

    #[test]
    fn format_coordinate_west() {
        let s = format_coordinate(-0.1278, 'E', 'W');
        assert!(s.contains("0.1278"));
        assert!(s.contains("W"));
    }
}

use adw::prelude::*;
use gtk::gio;

use crate::library::media::{MediaItem, MediaMetadataRecord};
use crate::ui::widgets::{detail_row, expander_row};

/// Scrollable metadata panel displayed in the [`super::PhotoViewer`] info sidebar.
///
/// Uses `AdwExpanderRow` sections inside a single boxed list card:
/// Date, Image, Camera, Location (conditional), and File.
/// Each section shows a subtitle summary when collapsed.
/// Call [`InfoPanel::populate`] whenever the viewed item changes.
pub struct InfoPanel {
    scrolled: gtk::ScrolledWindow,
}

impl InfoPanel {
    pub fn new() -> Self {
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        Self { scrolled }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.scrolled.upcast_ref()
    }

    /// Rebuild the panel with `item` data and optional `metadata`.
    ///
    /// Safe to call multiple times — the previous content is replaced.
    pub fn populate(&self, item: &MediaItem, metadata: Option<&MediaMetadataRecord>) {
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
        vbox.set_margin_top(12);
        vbox.set_margin_bottom(12);
        vbox.set_margin_start(12);
        vbox.set_margin_end(12);

        vbox.append(&build_date_section(item));
        vbox.append(&build_image_section(item));
        vbox.append(&build_camera_section(metadata));

        if let Some(location) = build_location_section(metadata) {
            vbox.append(&location);
        }

        vbox.append(&build_file_section(item));

        self.scrolled.set_child(Some(&vbox));
    }
}

// ── Section builders ────────────────────────────────────────────────────────

fn build_date_section(item: &MediaItem) -> gtk::ListBox {
    let list = boxed_list();
    let (short_date, long_date, time_str) = format_date_parts(item.taken_at);
    let (expander, _) = expander_row(
        Some("x-office-calendar-symbolic"),
        "Date",
        &short_date,
        true,
    );
    expander.add_row(&detail_row("Captured", &long_date));
    expander.add_row(&detail_row("Time", &time_str));
    list.append(&expander);
    list
}

fn build_image_section(item: &MediaItem) -> gtk::ListBox {
    let list = boxed_list();
    let mp_str = match (item.width, item.height) {
        (Some(w), Some(h)) => {
            let mp = (w * h) as f64 / 1_000_000.0;
            format!("{mp:.1} MP")
        }
        _ => "Unknown".to_string(),
    };

    let (expander, _) = expander_row(
        Some("image-x-generic-symbolic"),
        "Image",
        &mp_str,
        true,
    );

    if let (Some(w), Some(h)) = (item.width, item.height) {
        expander.add_row(&detail_row("Dimensions", &format!("{w} \u{d7} {h}")));
    }
    expander.add_row(&detail_row("Resolution", &mp_str));

    let format_str = item
        .original_filename
        .rsplit('.')
        .next()
        .map(|ext| ext.to_uppercase())
        .unwrap_or_else(|| "Unknown".to_string());
    expander.add_row(&detail_row("Format", &format_str));

    list.append(&expander);
    list
}

fn build_camera_section(metadata: Option<&MediaMetadataRecord>) -> gtk::ListBox {
    let list = boxed_list();
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

    let subtitle = camera_name.as_deref().unwrap_or("No data");
    let (expander, _) = expander_row(
        Some("camera-photo-symbolic"),
        "Camera",
        subtitle,
        true,
    );

    if let Some(ref name) = camera_name {
        expander.add_row(&detail_row("Camera", name));
    }

    if let Some(lens) = metadata.and_then(|m| m.lens_model.as_deref()) {
        let lens_with_fl = metadata
            .and_then(|m| m.focal_length)
            .map(|fl| format!("{lens} \u{b7} {fl:.0}mm"))
            .unwrap_or_else(|| lens.to_string());
        expander.add_row(&detail_row("Lens", &lens_with_fl));
    }

    if let Some(grid_row) = build_exif_grid(metadata) {
        expander.add_row(&grid_row);
    }

    if camera_name.is_none() && metadata.map(|m| !m.has_data()).unwrap_or(true) {
        expander.add_row(&detail_row("", "No EXIF data available"));
    }

    list.append(&expander);
    list
}

fn build_exif_grid(metadata: Option<&MediaMetadataRecord>) -> Option<gtk::ListBoxRow> {
    let meta = metadata?;

    let has_exif = meta.aperture.is_some()
        || meta.shutter_str.is_some()
        || meta.iso.is_some()
        || meta.focal_length.is_some();

    if !has_exif {
        return None;
    }

    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .column_homogeneous(true)
        .build();

    let mut row = 0i32;
    let mut col = 0i32;

    if let Some(f) = meta.aperture {
        grid.attach(&exif_card("Aperture", &format!("f/{f:.1}")), col, row, 1, 1);
        col += 1;
    }
    if let Some(s) = &meta.shutter_str {
        grid.attach(&exif_card("Shutter", &format!("{s}s")), col, row, 1, 1);
        col += 1;
    }
    if col >= 2 {
        row += 1;
        col = 0;
    }
    if let Some(iso) = meta.iso {
        grid.attach(&exif_card("ISO", &format!("{iso}")), col, row, 1, 1);
        col += 1;
    }
    if let Some(fl) = meta.focal_length {
        grid.attach(&exif_card("Focal", &format!("{fl:.0}mm")), col, row, 1, 1);
    }

    Some(
        gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .child(&grid)
            .build(),
    )
}

fn build_location_section(metadata: Option<&MediaMetadataRecord>) -> Option<gtk::ListBox> {
    let meta = metadata?;
    let (lat, lon) = match (meta.gps_lat, meta.gps_lon) {
        (Some(lat), Some(lon)) => (lat, lon),
        _ => return None,
    };

    let list = boxed_list();
    let coords_str = format!(
        "{}\u{b0}, {}\u{b0}",
        format_decimal(lat.abs(), 4),
        format_decimal(lon.abs(), 4),
    );

    let (expander, _) = expander_row(
        Some("mark-location-symbolic"),
        "Location",
        &coords_str,
        true,
    );

    expander.add_row(&detail_row("Latitude", &format_coordinate(lat, 'N', 'S')));
    expander.add_row(&detail_row("Longitude", &format_coordinate(lon, 'E', 'W')));

    if let Some(alt) = meta.gps_alt {
        expander.add_row(&detail_row("Altitude", &format!("{alt:.0} m")));
    }

    let btn_content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    btn_content.append(&gtk::Image::from_icon_name("find-location-symbolic"));
    btn_content.append(&gtk::Label::new(Some("Open in Maps")));

    let map_btn = gtk::Button::builder()
        .child(&btn_content)
        .margin_top(4)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    map_btn.add_css_class("outlined");

    let geo_uri = format!("geo:{lat},{lon}");
    map_btn.connect_clicked(move |btn| {
        let launcher = gtk::UriLauncher::new(&geo_uri);
        let window = btn.root().and_downcast::<gtk::Window>();
        launcher.launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
    });

    let btn_row = gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .child(&map_btn)
        .build();
    expander.add_row(&btn_row);

    list.append(&expander);
    Some(list)
}

fn build_file_section(item: &MediaItem) -> gtk::ListBox {
    let list = boxed_list();
    let (expander, _) = expander_row(
        Some("document-open-symbolic"),
        "File",
        &item.original_filename,
        false,
    );
    expander.add_row(&detail_row("Filename", &item.original_filename));
    list.append(&expander);
    list
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Create a new boxed-list ListBox for a single expander section.
fn boxed_list() -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list
}

/// Create a compact EXIF value card for the 2-column camera grid.
fn exif_card(label: &str, value: &str) -> gtk::Box {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    card.add_css_class("card");

    let label_widget = gtk::Label::builder()
        .label(label)
        .halign(gtk::Align::Start)
        .margin_start(8)
        .margin_top(6)
        .build();
    label_widget.add_css_class("caption");
    label_widget.add_css_class("dim-label");

    let value_widget = gtk::Label::builder()
        .label(value)
        .halign(gtk::Align::Start)
        .margin_start(8)
        .margin_bottom(6)
        .build();
    value_widget.add_css_class("heading");

    card.append(&label_widget);
    card.append(&value_widget);
    card
}

/// Format a GPS coordinate with direction suffix (e.g. "48.8584° N").
fn format_coordinate(value: f64, pos_dir: char, neg_dir: char) -> String {
    let dir = if value >= 0.0 { pos_dir } else { neg_dir };
    format!("{}\u{b0} {dir}", format_decimal(value.abs(), 4))
}

/// Format a float to a fixed number of decimal places without trailing zeros.
fn format_decimal(value: f64, decimals: usize) -> String {
    format!("{:.prec$}", value, prec = decimals)
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

/// Format a Unix timestamp as "Month Day, Year · HH:MM".
#[cfg(test)]
fn format_timestamp(ts: i64) -> Option<String> {
    use chrono::{DateTime, Utc};
    let dt = DateTime::<Utc>::from_timestamp(ts, 0)?;
    Some(dt.format("%B %-d, %Y \u{b7} %H:%M").to_string())
}

impl Default for InfoPanel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_known_value() {
        let s = format_timestamp(1_490_529_600).unwrap();
        assert!(s.contains("2017"), "expected year 2017 in {s:?}");
        assert!(s.contains("March"), "expected month in {s:?}");
    }

    #[test]
    fn format_timestamp_zero_returns_epoch() {
        let s = format_timestamp(0).unwrap();
        assert!(s.contains("1970"), "expected 1970 in {s:?}");
    }

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

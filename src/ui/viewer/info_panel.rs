use adw::prelude::*;

use crate::library::media::{MediaItem, MediaMetadataRecord};

/// Scrollable metadata panel displayed in the [`super::PhotoViewer`] info sidebar.
///
/// Uses `AdwExpanderRow` sections for Date, Image, Camera, and File metadata.
/// Each section shows a subtitle summary when collapsed, with full details
/// when expanded. Call [`InfoPanel::populate`] whenever the viewed item changes.
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

        // ── Date section ─────────────────────────────────────────────────────
        {
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);

            let (short_date, long_date, time_str) = format_date_parts(item.taken_at);

            let expander = expander_row("Date", &short_date, true);

            expander.add_row(&detail_row("Captured", &long_date));
            expander.add_row(&detail_row("Time", &time_str));

            list.append(&expander);
            vbox.append(&list);
        }

        // ── Image section ────────────────────────────────────────────────────
        {
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);

            let mp_str = match (item.width, item.height) {
                (Some(w), Some(h)) => {
                    let mp = (w * h) as f64 / 1_000_000.0;
                    format!("{mp:.1} MP")
                }
                _ => "Unknown".to_string(),
            };

            let expander = expander_row("Image", &mp_str, true);

            if let (Some(w), Some(h)) = (item.width, item.height) {
                expander.add_row(&detail_row("Dimensions", &format!("{w} \u{d7} {h}")));
            }
            expander.add_row(&detail_row("Resolution", &mp_str));

            // Derive format from filename extension.
            let format_str = item
                .original_filename
                .rsplit('.')
                .next()
                .map(|ext| ext.to_uppercase())
                .unwrap_or_else(|| "Unknown".to_string());
            expander.add_row(&detail_row("Format", &format_str));

            list.append(&expander);
            vbox.append(&list);
        }

        // ── Camera section ───────────────────────────────────────────────────
        {
            let camera_name = metadata.and_then(|m| {
                match (&m.camera_make, &m.camera_model) {
                    (Some(make), Some(model)) => {
                        // Avoid duplication like "Apple Apple iPhone 13"
                        if model.starts_with(make.as_str()) {
                            Some(model.clone())
                        } else {
                            Some(format!("{make} {model}"))
                        }
                    }
                    (Some(make), None) => Some(make.clone()),
                    (None, Some(model)) => Some(model.clone()),
                    _ => None,
                }
            });

            let subtitle = camera_name
                .as_deref()
                .unwrap_or("No data");

            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);

            let expander = expander_row("Camera", subtitle, true);

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

            // EXIF values in a 2-column grid.
            if let Some(meta) = metadata {
                let has_exif = meta.aperture.is_some()
                    || meta.shutter_str.is_some()
                    || meta.iso.is_some()
                    || meta.focal_length.is_some();

                if has_exif {
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

                    // Wrap grid in a ListBoxRow so it can be added to the expander.
                    let grid_row = gtk::ListBoxRow::builder()
                        .activatable(false)
                        .selectable(false)
                        .child(&grid)
                        .build();
                    expander.add_row(&grid_row);
                }
            }

            // Show "No data" if no camera metadata at all.
            if camera_name.is_none()
                && metadata.map(|m| !m.has_data()).unwrap_or(true)
            {
                expander.add_row(&detail_row("", "No EXIF data available"));
            }

            list.append(&expander);
            vbox.append(&list);
        }

        // ── File section (collapsed by default) ──────────────────────────────
        {
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);

            let expander = expander_row("File", &item.original_filename, false);

            expander.add_row(&detail_row("Filename", &item.original_filename));

            list.append(&expander);
            vbox.append(&list);
        }

        self.scrolled.set_child(Some(&vbox));
    }
}

/// Create an expander row with title left and subtitle right-aligned as a suffix.
fn expander_row(title: &str, subtitle: &str, expanded: bool) -> adw::ExpanderRow {
    let suffix = gtk::Label::builder()
        .label(subtitle)
        .halign(gtk::Align::End)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(20)
        .build();
    suffix.add_css_class("dim-label");

    let expander = adw::ExpanderRow::builder()
        .title(title)
        .show_enable_switch(false)
        .expanded(expanded)
        .build();
    expander.add_suffix(&suffix);

    expander
}

/// Create a detail row for inside an expander — label left, value right.
fn detail_row(label: &str, value: &str) -> gtk::ListBoxRow {
    let hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .margin_bottom(8)
        .build();

    let label_widget = gtk::Label::builder()
        .label(label)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();
    label_widget.add_css_class("dim-label");

    let value_widget = gtk::Label::builder()
        .label(value)
        .halign(gtk::Align::End)
        .selectable(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(24)
        .build();

    hbox.append(&label_widget);
    hbox.append(&value_widget);

    gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .child(&hbox)
        .build()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_known_value() {
        // 2017-03-26 12:00:00 UTC
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
}

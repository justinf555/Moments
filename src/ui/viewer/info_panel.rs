use adw::prelude::*;

use crate::library::media::{MediaItem, MediaMetadataRecord};

/// Scrollable metadata panel displayed in the [`super::PhotoViewer`] info sidebar.
///
/// Call [`InfoPanel::populate`] whenever the viewed item or its loaded metadata
/// changes. The panel rebuilds its content in place each time.
pub struct InfoPanel {
    scrolled: gtk::ScrolledWindow,
}

impl InfoPanel {
    pub fn new() -> Self {
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(280)
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
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.set_margin_top(12);
        vbox.set_margin_bottom(12);
        vbox.set_margin_start(12);
        vbox.set_margin_end(12);

        // ── Date ─────────────────────────────────────────────────────────────
        let date_group = adw::PreferencesGroup::builder().title("Date").build();
        let date_str = item
            .taken_at
            .and_then(format_timestamp)
            .unwrap_or_else(|| "Unknown".to_string());
        date_group.add(&action_row("Captured", &date_str));
        vbox.append(&date_group);

        // ── Image ─────────────────────────────────────────────────────────────
        if item.width.is_some() || item.height.is_some() {
            let img_group = adw::PreferencesGroup::builder().title("Image").build();
            if let (Some(w), Some(h)) = (item.width, item.height) {
                let mp = (w * h) as f64 / 1_000_000.0;
                img_group.add(&action_row("Dimensions", &format!("{w} × {h}  ({mp:.1} MP)")));
            }
            if let Some(cs) = metadata.and_then(|m| m.color_space.as_deref()) {
                img_group.add(&action_row("Color Space", cs));
            }
            vbox.append(&img_group);
        }

        // ── Camera ───────────────────────────────────────────────────────────
        if let Some(meta) = metadata {
            let has_camera = meta.camera_make.is_some()
                || meta.camera_model.is_some()
                || meta.lens_model.is_some()
                || meta.aperture.is_some()
                || meta.shutter_str.is_some()
                || meta.iso.is_some()
                || meta.focal_length.is_some();

            if has_camera {
                let cam_group = adw::PreferencesGroup::builder().title("Camera").build();

                let camera_name = match (&meta.camera_make, &meta.camera_model) {
                    (Some(make), Some(model)) => Some(format!("{make} {model}")),
                    (Some(make), None) => Some(make.clone()),
                    (None, Some(model)) => Some(model.clone()),
                    _ => None,
                };
                if let Some(name) = camera_name {
                    cam_group.add(&action_row("Camera", &name));
                }
                if let Some(lens) = &meta.lens_model {
                    cam_group.add(&action_row("Lens", lens));
                }
                if let Some(f) = meta.aperture {
                    cam_group.add(&action_row("Aperture", &format!("f/{f:.1}")));
                }
                if let Some(s) = &meta.shutter_str {
                    cam_group.add(&action_row("Shutter", &format!("{s}s")));
                }
                if let Some(iso) = meta.iso {
                    cam_group.add(&action_row("ISO", &format!("{iso}")));
                }
                if let Some(fl) = meta.focal_length {
                    cam_group.add(&action_row("Focal Length", &format!("{fl:.0}mm")));
                }
                vbox.append(&cam_group);
            }
        }

        // ── File ─────────────────────────────────────────────────────────────
        let file_group = adw::PreferencesGroup::builder().title("File").build();
        file_group.add(&action_row("Filename", &item.original_filename));
        vbox.append(&file_group);

        self.scrolled.set_child(Some(&vbox));
    }
}

fn action_row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build()
}

/// Format a Unix timestamp as "Month Day, Year · HH:MM".
fn format_timestamp(ts: i64) -> Option<String> {
    use chrono::{DateTime, Utc};
    let dt = DateTime::<Utc>::from_timestamp(ts, 0)?;
    Some(dt.format("%B %-d, %Y · %H:%M").to_string())
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
}

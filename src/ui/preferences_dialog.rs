use adw::prelude::*;
use gtk::{gio, glib};
use std::sync::Arc;
use tracing::debug;

use crate::library::Library;

/// Build and present the Preferences dialog.
///
/// Uses `AdwPreferencesDialog` with up to 3 pages:
/// - General (theme, library settings)
/// - Library (stats overview, storage)
/// - Immich (connection info, sync settings) — only for Immich backends
pub fn show_preferences(
    window: &impl IsA<gtk::Widget>,
    settings: &gio::Settings,
    is_immich: bool,
    library: Option<Arc<dyn Library>>,
    immich_server_url: Option<String>,
) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");

    // ── General page ────────────────────────────────────────────────────
    let general_page = adw::PreferencesPage::new();
    general_page.set_title("General");
    general_page.set_icon_name(Some("preferences-system-symbolic"));

    // Appearance group
    let appearance_group = adw::PreferencesGroup::new();
    appearance_group.set_title("Appearance");

    let theme_row = adw::ComboRow::new();
    theme_row.set_title("Color Scheme");
    let themes = gtk::StringList::new(&["Follow System", "Light", "Dark"]);
    theme_row.set_model(Some(&themes));

    // Map GSettings value to combo index: 0→0 (default), 1→1 (light), 4→2 (dark)
    let current = settings.uint("color-scheme");
    let idx = match current {
        1 => 1u32,
        4 => 2,
        _ => 0,
    };
    theme_row.set_selected(idx);

    let settings_theme = settings.clone();
    theme_row.connect_selected_notify(move |row| {
        let value = match row.selected() {
            1 => 1u32, // force light
            2 => 4,    // force dark
            _ => 0,    // default/system
        };
        let _ = settings_theme.set_uint("color-scheme", value);
        let scheme = match value {
            1 => adw::ColorScheme::ForceLight,
            4 => adw::ColorScheme::ForceDark,
            _ => adw::ColorScheme::Default,
        };
        adw::StyleManager::default().set_color_scheme(scheme);
        debug!(color_scheme = value, "theme changed");
    });
    appearance_group.add(&theme_row);
    general_page.add(&appearance_group);

    // Library group
    let library_group = adw::PreferencesGroup::new();
    library_group.set_title("Library");

    let recent_row = adw::SpinRow::new(
        Some(&gtk::Adjustment::new(30.0, 0.0, 365.0, 1.0, 10.0, 0.0)),
        1.0,
        0,
    );
    recent_row.set_title("Recent Imports");
    recent_row.set_subtitle("Days to show in Recent Imports view");
    recent_row.set_value(settings.uint("recent-imports-days") as f64);
    let settings_recent = settings.clone();
    recent_row.connect_changed(move |row| {
        let _ = settings_recent.set_uint("recent-imports-days", row.value() as u32);
    });
    library_group.add(&recent_row);

    let trash_row = adw::SpinRow::new(
        Some(&gtk::Adjustment::new(30.0, 1.0, 365.0, 1.0, 10.0, 0.0)),
        1.0,
        0,
    );
    trash_row.set_title("Trash Retention");
    trash_row.set_subtitle("Days before trashed items are permanently deleted");
    trash_row.set_value(settings.uint("trash-retention-days") as f64);
    let settings_trash = settings.clone();
    trash_row.connect_changed(move |row| {
        let _ = settings_trash.set_uint("trash-retention-days", row.value() as u32);
    });
    library_group.add(&trash_row);
    general_page.add(&library_group);

    dialog.add(&general_page);

    // ── Library page ────────────────────────────────────────────────────
    let library_page = adw::PreferencesPage::new();
    library_page.set_title("Library");
    library_page.set_icon_name(Some("folder-symbolic"));

    let overview_group = adw::PreferencesGroup::new();
    overview_group.set_title("Overview");

    let photos_row = adw::ActionRow::new();
    photos_row.set_title("Photos");
    photos_row.set_subtitle("Loading...");
    overview_group.add(&photos_row);

    let videos_row = adw::ActionRow::new();
    videos_row.set_title("Videos");
    videos_row.set_subtitle("Loading...");
    overview_group.add(&videos_row);

    let albums_row = adw::ActionRow::new();
    albums_row.set_title("Albums");
    albums_row.set_subtitle("Loading...");
    overview_group.add(&albums_row);

    library_page.add(&overview_group);

    // Storage group
    let storage_group = adw::PreferencesGroup::new();
    storage_group.set_title("Storage");

    if is_immich {
        let cache_row = adw::SpinRow::new(
            Some(&gtk::Adjustment::new(2048.0, 0.0, 50000.0, 256.0, 1024.0, 0.0)),
            256.0,
            0,
        );
        cache_row.set_title("Originals Cache Limit");
        cache_row.set_subtitle("Maximum disk cache for downloaded originals (MB)");
        cache_row.set_value(settings.uint("originals-cache-max-mb") as f64);
        let settings_cache = settings.clone();
        cache_row.connect_changed(move |row| {
            let _ = settings_cache.set_uint("originals-cache-max-mb", row.value() as u32);
        });
        storage_group.add(&cache_row);
    }

    library_page.add(&storage_group);
    dialog.add(&library_page);

    // Load stats async.
    if let Some(lib) = library.clone() {
        let tokio = crate::application::MomentsApplication::default().tokio_handle();
        let photos_weak = photos_row.downgrade();
        let videos_weak = videos_row.downgrade();
        let albums_weak = albums_row.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let result = tokio
                .spawn(async move { lib.library_stats().await })
                .await;
            match result {
                Ok(Ok(stats)) => {
                    if let Some(r) = photos_weak.upgrade() {
                        r.set_subtitle(&format_count(stats.photo_count, "photo"));
                    }
                    if let Some(r) = videos_weak.upgrade() {
                        r.set_subtitle(&format_count(stats.video_count, "video"));
                    }
                    if let Some(r) = albums_weak.upgrade() {
                        r.set_subtitle(&format_count(stats.album_count, "album"));
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("library_stats failed: {e}");
                    if let Some(r) = photos_weak.upgrade() { r.set_subtitle("—"); }
                    if let Some(r) = videos_weak.upgrade() { r.set_subtitle("—"); }
                    if let Some(r) = albums_weak.upgrade() { r.set_subtitle("—"); }
                }
                Err(e) => {
                    tracing::error!("library_stats join failed: {e}");
                }
            }
        });
    }

    // ── Immich page (conditional) ───────────────────────────────────────
    if is_immich {
        let immich_page = adw::PreferencesPage::new();
        immich_page.set_title("Immich");
        immich_page.set_icon_name(Some("network-server-symbolic"));

        // Connection group
        let conn_group = adw::PreferencesGroup::new();
        conn_group.set_title("Connection");

        if let Some(ref url) = immich_server_url {
            let server_row = adw::ActionRow::new();
            server_row.set_title("Server");
            server_row.set_subtitle(url);
            conn_group.add(&server_row);

            // Open Immich Web button
            let open_btn = gtk::Button::with_label("Open Immich Web");
            open_btn.add_css_class("flat");
            open_btn.set_halign(gtk::Align::Start);
            open_btn.set_margin_top(8);
            let url_clone = url.clone();
            open_btn.connect_clicked(move |_| {
                let _ = gio::AppInfo::launch_default_for_uri(&url_clone, gio::AppLaunchContext::NONE);
            });
            conn_group.add(&open_btn);
        }

        immich_page.add(&conn_group);

        // Sync group
        let sync_group = adw::PreferencesGroup::new();
        sync_group.set_title("Sync");

        let interval_row = adw::SpinRow::new(
            Some(&gtk::Adjustment::new(30.0, 5.0, 3600.0, 5.0, 30.0, 0.0)),
            5.0,
            0,
        );
        interval_row.set_title("Polling Interval");
        interval_row.set_subtitle("Seconds between sync cycles");
        interval_row.set_value(settings.uint("sync-interval-seconds") as f64);
        let settings_sync = settings.clone();
        interval_row.connect_changed(move |row| {
            let _ = settings_sync.set_uint("sync-interval-seconds", row.value() as u32);
        });
        sync_group.add(&interval_row);

        immich_page.add(&sync_group);
        dialog.add(&immich_page);
    }

    dialog.present(Some(window));
}

/// Format a count with singular/plural label (e.g. "1,976 photos").
fn format_count(count: u64, singular: &str) -> String {
    let label = if count == 1 { singular } else { &format!("{singular}s") };
    format!("{} {label}", format_number(count))
}

/// Format a number with thousands separators (e.g. 1976 → "1,976").
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_count_singular() {
        assert_eq!(format_count(1, "photo"), "1 photo");
    }

    #[test]
    fn format_count_plural() {
        assert_eq!(format_count(1976, "photo"), "1,976 photos");
    }

    #[test]
    fn format_count_zero() {
        assert_eq!(format_count(0, "album"), "0 albums");
    }

    #[test]
    fn format_number_small() {
        assert_eq!(format_number(42), "42");
    }

    #[test]
    fn format_number_thousands() {
        assert_eq!(format_number(1976), "1,976");
    }

    #[test]
    fn format_number_millions() {
        assert_eq!(format_number(1234567), "1,234,567");
    }
}

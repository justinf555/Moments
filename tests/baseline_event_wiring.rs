//! Phase 0: Baseline tests for event wiring before the event bus migration.
//!
//! These tests cover the current ModelRegistry → PhotoGridModel broadcast
//! pattern, MediaItemObject property updates, and PhotoGridCell widget state.
//! They serve as regression tests through every phase of #230.
//!
//! Run with:
//!   cargo test --features integration-tests --test baseline_event_wiring -- --test-threads=1

#![cfg(feature = "integration-tests")]

#[allow(dead_code)]
mod common;

use gtk::prelude::*;

use moments::client::MediaItemObject;
use moments::library::media::{MediaId, MediaItem, MediaType};

/// Create a test MediaItem with the given ID and sensible defaults.
fn test_item(id: &str) -> MediaItem {
    MediaItem {
        id: MediaId::new(id.to_string()),
        taken_at: Some(1000),
        imported_at: 1000,
        original_filename: format!("{id}.jpg"),
        width: Some(640),
        height: Some(480),
        orientation: 1,
        media_type: MediaType::Image,
        is_favorite: false,
        is_trashed: false,
        trashed_at: None,
        duration_ms: None,
    }
}

// ── MediaItemObject property tests ──────────────────────────────────────────

#[cfg(test)]
mod media_item_object {
    use super::*;

    #[gtk::test]
    fn new_sets_properties_from_media_item() {
        let mut item = test_item("abc123");
        item.is_favorite = true;
        item.trashed_at = Some(999);
        item.duration_ms = Some(5000);

        let obj = MediaItemObject::new(item);

        assert!(obj.is_favorite());
        assert_eq!(obj.trashed_at(), 999);
        assert_eq!(obj.duration_ms(), 5000);
        assert!(obj.texture().is_none());
    }

    #[gtk::test]
    fn favorite_property_fires_notify() {
        let obj = MediaItemObject::new(test_item("abc123"));
        assert!(!obj.is_favorite());

        let notified = std::rc::Rc::new(std::cell::Cell::new(false));
        let n = notified.clone();
        obj.connect_is_favorite_notify(move |_| {
            n.set(true);
        });

        obj.set_is_favorite(true);
        assert!(obj.is_favorite());
        assert!(notified.get(), "notify::is-favorite should have fired");
    }

    #[gtk::test]
    fn texture_property_fires_notify() {
        let obj = MediaItemObject::new(test_item("abc123"));

        let notified = std::rc::Rc::new(std::cell::Cell::new(false));
        let n = notified.clone();
        obj.connect_texture_notify(move |_| {
            n.set(true);
        });

        let bytes = gtk::glib::Bytes::from_owned(vec![255u8, 0, 0, 255]);
        let texture =
            gtk::gdk::MemoryTexture::new(1, 1, gtk::gdk::MemoryFormat::R8g8b8a8, &bytes, 4);
        obj.set_texture(Some(texture.upcast::<gtk::gdk::Texture>()));
        assert!(obj.texture().is_some());
        assert!(notified.get(), "notify::texture should have fired");
    }

    #[gtk::test]
    fn item_returns_underlying_media_item() {
        let item = test_item("media-42");
        let obj = MediaItemObject::new(item);
        assert_eq!(obj.item().id.as_str(), "media-42");
        assert_eq!(obj.item().original_filename, "media-42.jpg");
    }
}

// PhotoGridModel and bus_broadcast tests removed — PhotoGridModel was deleted
// during the layered architecture refactor. These tests will be replaced by
// MediaClient-level tests when the EventBus migration is complete.

// ── PhotoGridCell widget tests ──────────────────────────────────────────────
// Note: imp() is not accessible from outside the crate, so these tests
// use only public methods. The PoC tests in headless_poc.rs already cover
// basic cell creation — these test the selection mode contract.

#[cfg(test)]
mod photo_grid_cell {
    use crate::common::resources::ensure_resources;
    use moments::ui::photo_grid::cell::PhotoGridCell;

    #[gtk::test]
    fn new_cell_can_be_constructed() {
        ensure_resources();
        let _cell = PhotoGridCell::new();
        // If this doesn't panic, the GObject subclass is correctly registered
    }

    #[gtk::test]
    fn selection_mode_roundtrip() {
        ensure_resources();
        let cell = PhotoGridCell::new();
        // These are public methods — we verify they don't panic
        cell.set_selection_mode(true);
        cell.set_selection_mode(false);
    }

    #[gtk::test]
    fn set_checked_roundtrip() {
        ensure_resources();
        let cell = PhotoGridCell::new();
        cell.set_checked(true);
        cell.set_checked(false);
    }
}

//! Phase 0: Baseline tests for event wiring before the event bus migration.
//!
//! These tests cover the current ModelRegistry → PhotoGridModel broadcast
//! pattern, MediaItemObject property updates, and PhotoGridCell widget state.
//! They serve as regression tests through every phase of #230.
//!
//! Run with:
//!   cargo test --features integration-tests --test baseline_event_wiring -- --test-threads=1

#![cfg(feature = "integration-tests")]

mod common;

use std::rc::Rc;

use gtk::prelude::*;

use moments::app_event::AppEvent;
use moments::event_bus::EventBus;
use moments::library::media::{MediaFilter, MediaId, MediaItem, MediaType};
use moments::ui::photo_grid::item::MediaItemObject;
use moments::ui::photo_grid::model::PhotoGridModel;

use common::mock_library::stub_deps;

/// Process all pending GLib main loop events.
fn flush_events() {
    while gtk::glib::MainContext::default().iteration(false) {}
}

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

/// Create a test MediaItem with a specific taken_at timestamp.
fn test_item_at(id: &str, taken_at: i64) -> MediaItem {
    let mut item = test_item(id);
    item.taken_at = Some(taken_at);
    item
}

fn make_model(filter: MediaFilter) -> Rc<PhotoGridModel> {
    let (lib, tokio) = stub_deps();
    Rc::new(PhotoGridModel::new(lib, tokio, filter, moments::event_bus::EventSender::no_op()))
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
        let texture = gtk::gdk::MemoryTexture::new(
            1, 1,
            gtk::gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            4,
        );
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

// ── PhotoGridModel synchronous operations ───────────────────────────────────

#[cfg(test)]
mod photo_grid_model {
    use super::*;

    #[gtk::test]
    fn insert_item_sorted_adds_to_store() {
        let model = make_model(MediaFilter::All);
        model.insert_item_sorted(test_item("id-1"));
        assert_eq!(model.store.n_items(), 1);

        model.insert_item_sorted(test_item("id-2"));
        assert_eq!(model.store.n_items(), 2);
    }

    #[gtk::test]
    fn insert_item_sorted_deduplicates() {
        let model = make_model(MediaFilter::All);
        model.insert_item_sorted(test_item("dup-id"));
        model.insert_item_sorted(test_item("dup-id"));
        assert_eq!(model.store.n_items(), 1, "duplicate should be skipped");
    }

    #[gtk::test]
    fn insert_item_sorted_maintains_descending_order() {
        let model = make_model(MediaFilter::All);

        model.insert_item_sorted(test_item_at("old", 1000));
        model.insert_item_sorted(test_item_at("new", 3000));
        model.insert_item_sorted(test_item_at("mid", 2000));

        let first = model.store.item(0).unwrap().downcast::<MediaItemObject>().unwrap();
        let second = model.store.item(1).unwrap().downcast::<MediaItemObject>().unwrap();
        let third = model.store.item(2).unwrap().downcast::<MediaItemObject>().unwrap();

        assert_eq!(first.item().id.as_str(), "new");
        assert_eq!(second.item().id.as_str(), "mid");
        assert_eq!(third.item().id.as_str(), "old");
    }

    #[gtk::test]
    fn on_deleted_removes_item_from_store() {
        let model = make_model(MediaFilter::All);
        model.insert_item_sorted(test_item("keep"));
        model.insert_item_sorted(test_item("delete-me"));
        assert_eq!(model.store.n_items(), 2);

        model.on_deleted(&MediaId::new("delete-me".to_string()));
        assert_eq!(model.store.n_items(), 1);

        let remaining = model.store.item(0).unwrap().downcast::<MediaItemObject>().unwrap();
        assert_eq!(remaining.item().id.as_str(), "keep");
    }

    #[gtk::test]
    fn on_deleted_noop_for_missing_id() {
        let model = make_model(MediaFilter::All);
        model.insert_item_sorted(test_item("exists"));

        model.on_deleted(&MediaId::new("not-in-store".to_string()));
        assert_eq!(model.store.n_items(), 1, "store should be unchanged");
    }

    #[gtk::test]
    fn on_favorite_changed_updates_property_in_all_filter() {
        let model = make_model(MediaFilter::All);
        model.insert_item_sorted(test_item("fav-test"));

        let id = MediaId::new("fav-test".to_string());
        model.on_favorite_changed(&id, true);

        let obj = model.store.item(0).unwrap().downcast::<MediaItemObject>().unwrap();
        assert!(obj.is_favorite(), "should be marked as favorite");

        model.on_favorite_changed(&id, false);
        let obj = model.store.item(0).unwrap().downcast::<MediaItemObject>().unwrap();
        assert!(!obj.is_favorite(), "should be unmarked");
    }

    #[gtk::test]
    fn on_favorite_changed_removes_unfavorited_from_favorites_view() {
        let model = make_model(MediaFilter::Favorites);

        let mut item = test_item("fav-item");
        item.is_favorite = true;
        model.insert_item_sorted(item);
        assert_eq!(model.store.n_items(), 1);

        model.on_favorite_changed(&MediaId::new("fav-item".to_string()), false);
        assert_eq!(model.store.n_items(), 0, "unfavorited item should be removed");
    }

    #[gtk::test]
    fn on_trashed_removes_from_all_filter() {
        let model = make_model(MediaFilter::All);
        model.insert_item_sorted(test_item("trash-me"));
        assert_eq!(model.store.n_items(), 1);

        model.on_trashed(&MediaId::new("trash-me".to_string()), true);
        assert_eq!(model.store.n_items(), 0, "trashed item removed from All");
    }

    #[gtk::test]
    fn on_trashed_removes_restored_from_trash_view() {
        let model = make_model(MediaFilter::Trashed);

        let mut item = test_item("in-trash");
        item.is_trashed = true;
        model.insert_item_sorted(item);
        assert_eq!(model.store.n_items(), 1);

        model.on_trashed(&MediaId::new("in-trash".to_string()), false);
        assert_eq!(model.store.n_items(), 0, "restored item removed from Trash");
    }

    #[gtk::test]
    fn on_favorite_noop_in_trashed_filter() {
        let model = make_model(MediaFilter::Trashed);
        let id = MediaId::new("nonexistent".to_string());
        model.on_favorite_changed(&id, true);
        assert_eq!(model.store.n_items(), 0);
    }

    #[gtk::test]
    fn filter_returns_construction_filter() {
        assert_eq!(make_model(MediaFilter::Favorites).filter(), MediaFilter::Favorites);
        assert_eq!(make_model(MediaFilter::Trashed).filter(), MediaFilter::Trashed);
        assert_eq!(make_model(MediaFilter::All).filter(), MediaFilter::All);
    }
}

// ── Bus-based broadcast tests (replaces ModelRegistry tests) ────────────────

#[cfg(test)]
mod bus_broadcast {
    use super::*;

    #[gtk::test]
    fn deleted_event_reaches_all_subscribed_models() {
        let bus = EventBus::new();
        let model_a = make_model(MediaFilter::All);
        let model_b = make_model(MediaFilter::All);

        model_a.subscribe(&bus);
        model_b.subscribe(&bus);

        model_a.insert_item_sorted(test_item("shared-id"));
        model_b.insert_item_sorted(test_item("shared-id"));

        bus.sender().send(AppEvent::Deleted {
            ids: vec![MediaId::new("shared-id".to_string())],
        });
        flush_events();

        assert_eq!(model_a.store.n_items(), 0);
        assert_eq!(model_b.store.n_items(), 0);
    }

    #[gtk::test]
    fn favorite_changed_reaches_all_subscribed_models() {
        let bus = EventBus::new();
        let model_a = make_model(MediaFilter::All);
        let model_b = make_model(MediaFilter::All);

        model_a.subscribe(&bus);
        model_b.subscribe(&bus);

        model_a.insert_item_sorted(test_item("fav-id"));
        model_b.insert_item_sorted(test_item("fav-id"));

        bus.sender().send(AppEvent::FavoriteChanged {
            ids: vec![MediaId::new("fav-id".to_string())],
            is_favorite: true,
        });
        flush_events();

        let obj_a = model_a.store.item(0).unwrap().downcast::<MediaItemObject>().unwrap();
        let obj_b = model_b.store.item(0).unwrap().downcast::<MediaItemObject>().unwrap();
        assert!(obj_a.is_favorite());
        assert!(obj_b.is_favorite());
    }

    #[gtk::test]
    fn trashed_event_removes_from_all_model() {
        let bus = EventBus::new();
        let model_all = make_model(MediaFilter::All);
        model_all.subscribe(&bus);

        model_all.insert_item_sorted(test_item("trash-id"));

        bus.sender().send(AppEvent::Trashed {
            ids: vec![MediaId::new("trash-id".to_string())],
        });
        flush_events();

        assert_eq!(model_all.store.n_items(), 0);
    }

    #[gtk::test]
    fn asset_synced_inserts_into_matching_models() {
        let bus = EventBus::new();
        let model_all = make_model(MediaFilter::All);
        let model_trash = make_model(MediaFilter::Trashed);

        model_all.subscribe(&bus);
        model_trash.subscribe(&bus);

        bus.sender().send(AppEvent::AssetSynced {
            item: test_item("synced-asset"),
        });
        flush_events();

        assert_eq!(model_all.store.n_items(), 1, "non-trashed → All");
        assert_eq!(model_trash.store.n_items(), 0, "non-trashed ≠ Trashed");
    }

    #[gtk::test]
    fn asset_synced_routes_trashed_to_trash_model() {
        let bus = EventBus::new();
        let model_all = make_model(MediaFilter::All);
        let model_trash = make_model(MediaFilter::Trashed);

        model_all.subscribe(&bus);
        model_trash.subscribe(&bus);

        let mut item = test_item("trashed-asset");
        item.is_trashed = true;
        bus.sender().send(AppEvent::AssetSynced { item });
        flush_events();

        assert_eq!(model_all.store.n_items(), 0, "trashed ≠ All");
        assert_eq!(model_trash.store.n_items(), 1, "trashed → Trashed");
    }
}

// ── PhotoGridCell widget tests ──────────────────────────────────────────────
// Note: imp() is not accessible from outside the crate, so these tests
// use only public methods. The PoC tests in headless_poc.rs already cover
// basic cell creation — these test the selection mode contract.

#[cfg(test)]
mod photo_grid_cell {
    use moments::ui::photo_grid::cell::PhotoGridCell;

    #[gtk::test]
    fn new_cell_can_be_constructed() {
        let _cell = PhotoGridCell::new();
        // If this doesn't panic, the GObject subclass is correctly registered
    }

    #[gtk::test]
    fn selection_mode_roundtrip() {
        let cell = PhotoGridCell::new();
        // These are public methods — we verify they don't panic
        cell.set_selection_mode(true);
        cell.set_selection_mode(false);
    }

    #[gtk::test]
    fn set_checked_roundtrip() {
        let cell = PhotoGridCell::new();
        cell.set_checked(true);
        cell.set_checked(false);
    }
}

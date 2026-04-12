use std::collections::HashSet;
use std::path::PathBuf;

use super::manager::SyncManager;
use super::types::*;
use super::SyncCounters;
use crate::library::album::{AlbumId, LibraryAlbums};
use crate::library::db::test_helpers::{get_audit_record, open_test_db};
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::event::LibraryEvent;
use crate::library::immich_client::ImmichClient;
use crate::library::media::{LibraryMedia, MediaId, MediaType};
use tempfile::tempdir;

/// Create a SyncManager with a real test DB for handler tests.
/// The ImmichClient points to a dummy URL — only tests that don't
/// call HTTP methods (handle_asset, handle_album, etc.) are safe.
async fn test_sync_manager(db: Database) -> (SyncManager, std::sync::mpsc::Receiver<LibraryEvent>) {
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (thumbnail_tx, _thumbnail_rx) = tokio::sync::mpsc::channel(100);
    let (interval_tx, interval_rx) = tokio::sync::watch::channel(60u64);
    // Keep senders alive so channels don't close.
    std::mem::forget(shutdown_tx);
    std::mem::forget(interval_tx);

    let client = ImmichClient::new("http://localhost:9999", "test-token").unwrap();
    let manager = SyncManager {
        client,
        db,
        events: event_tx,
        shutdown_rx,
        thumbnail_tx,
        thumbnails_dir: PathBuf::from("/tmp/test-thumbnails"),
        interval_rx: tokio::sync::Mutex::new(interval_rx),
    };
    (manager, event_rx)
}

// ── Helper function tests ───────────────────────────────────────────

#[test]
fn parse_datetime_valid() {
    let s = Some("2024-01-15T10:30:00.000Z".to_string());
    let ts = parse_datetime(&s).unwrap();
    assert!(ts > 0);
}

#[test]
fn parse_datetime_none() {
    assert!(parse_datetime(&None).is_none());
}

#[test]
fn parse_datetime_invalid() {
    let s = Some("not-a-date".to_string());
    assert!(parse_datetime(&s).is_none());
}

#[test]
fn parse_duration_ms_valid() {
    assert_eq!(parse_duration_ms("0:01:30.000000"), Some(90_000));
    assert_eq!(parse_duration_ms("1:00:00.000000"), Some(3_600_000));
    assert_eq!(parse_duration_ms("0:00:05.500000"), Some(5_500));
}

#[test]
fn parse_duration_ms_invalid() {
    assert!(parse_duration_ms("invalid").is_none());
    assert!(parse_duration_ms("").is_none());
}

// ── handle_asset tests ──────────────────────────────────────────────

#[tokio::test]
async fn handle_asset_upserts_image() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, events) = test_sync_manager(db.clone()).await;

    let asset = SyncAssetV1 {
        id: "asset-001".to_string(),
        original_file_name: "sunset.jpg".to_string(),
        asset_type: "IMAGE".to_string(),
        is_favorite: true,
        deleted_at: None,
        file_created_at: Some("2024-06-15T12:00:00.000Z".to_string()),
        local_date_time: Some("2024-06-15T14:00:00.000+02:00".to_string()),
        duration: None,
        width: Some(4032),
        height: Some(3024),
    };

    mgr.handle_asset(asset).await.unwrap();

    // Verify DB state.
    let id = MediaId::new("asset-001".to_string());
    assert!(db.media_exists(&id).await.unwrap());
    let item = db.get_media_item(&id).await.unwrap().unwrap();
    assert_eq!(item.original_filename, "sunset.jpg");
    assert!(item.is_favorite);
    assert!(!item.is_trashed);
    assert_eq!(item.width, Some(4032));
    assert_eq!(item.height, Some(3024));
    assert_eq!(item.media_type, MediaType::Image);
    assert!(item.taken_at.is_some());

    // Verify event emitted.
    let event = events.try_recv().unwrap();
    assert!(matches!(event, LibraryEvent::AssetSynced { .. }));
}

#[tokio::test]
async fn handle_asset_upserts_video_with_duration() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    let asset = SyncAssetV1 {
        id: "video-001".to_string(),
        original_file_name: "clip.mp4".to_string(),
        asset_type: "VIDEO".to_string(),
        is_favorite: false,
        deleted_at: None,
        file_created_at: Some("2024-03-01T08:00:00.000Z".to_string()),
        local_date_time: None,
        duration: Some("0:01:30.000000".to_string()),
        width: Some(1920),
        height: Some(1080),
    };

    mgr.handle_asset(asset).await.unwrap();

    let id = MediaId::new("video-001".to_string());
    let item = db.get_media_item(&id).await.unwrap().unwrap();
    assert_eq!(item.media_type, MediaType::Video);
    assert_eq!(item.duration_ms, Some(90_000));
}

#[tokio::test]
async fn handle_asset_trashed_item() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    let asset = SyncAssetV1 {
        id: "trashed-001".to_string(),
        original_file_name: "deleted.jpg".to_string(),
        asset_type: "IMAGE".to_string(),
        is_favorite: false,
        deleted_at: Some("2024-07-01T00:00:00.000Z".to_string()),
        file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
        local_date_time: None,
        duration: None,
        width: None,
        height: None,
    };

    mgr.handle_asset(asset).await.unwrap();

    let id = MediaId::new("trashed-001".to_string());
    let item = db.get_media_item(&id).await.unwrap().unwrap();
    assert!(item.is_trashed);
    assert!(item.trashed_at.is_some());
}

// ── handle_asset_exif tests ─────────────────────────────────────────

#[tokio::test]
async fn handle_asset_exif_upserts_metadata() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    // First insert the asset so the FK exists.
    let asset = SyncAssetV1 {
        id: "exif-asset".to_string(),
        original_file_name: "photo.jpg".to_string(),
        asset_type: "IMAGE".to_string(),
        is_favorite: false,
        deleted_at: None,
        file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
        local_date_time: None,
        duration: None,
        width: Some(4000),
        height: Some(3000),
    };
    mgr.handle_asset(asset).await.unwrap();

    let exif = SyncAssetExifV1 {
        asset_id: "exif-asset".to_string(),
        make: Some("Canon".to_string()),
        model: Some("EOS R5".to_string()),
        lens_model: Some("RF 24-70mm F2.8".to_string()),
        f_number: Some(2.8),
        exposure_time: Some("1/250".to_string()),
        iso: Some(400),
        focal_length: Some(50.0),
        latitude: Some(51.5074),
        longitude: Some(-0.1278),
        profile_description: Some("sRGB".to_string()),
    };

    // Should succeed without error — metadata is stored in the DB.
    mgr.handle_asset_exif(exif).await.unwrap();
}

// ── handle_album tests ──────────────────────────────────────────────

#[tokio::test]
async fn handle_album_upserts_album() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, events) = test_sync_manager(db.clone()).await;

    let album = SyncAlbumV1 {
        id: "album-001".to_string(),
        name: "Holiday 2024".to_string(),
        created_at: "2024-06-01T00:00:00.000Z".to_string(),
        updated_at: "2024-06-15T00:00:00.000Z".to_string(),
    };

    mgr.handle_album(album).await.unwrap();

    let albums = db.list_albums().await.unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].name, "Holiday 2024");
    assert_eq!(albums[0].id.as_str(), "album-001");

    let event = events.try_recv().unwrap();
    assert!(matches!(event, LibraryEvent::AlbumCreated { .. }));
}

#[tokio::test]
async fn handle_album_delete_removes_album() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    // Create then delete.
    let album = SyncAlbumV1 {
        id: "album-del".to_string(),
        name: "To Delete".to_string(),
        created_at: "2024-01-01T00:00:00.000Z".to_string(),
        updated_at: "2024-01-01T00:00:00.000Z".to_string(),
    };
    mgr.handle_album(album).await.unwrap();
    assert_eq!(db.list_albums().await.unwrap().len(), 1);

    mgr.handle_album_delete("album-del").await.unwrap();
    assert!(db.list_albums().await.unwrap().is_empty());
}

// ── handle_asset_face tests ─────────────────────────────────────────

#[tokio::test]
async fn handle_asset_face_upserts_face_and_updates_count() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    // Create the asset first (FK constraint).
    let asset = SyncAssetV1 {
        id: "face-asset".to_string(),
        original_file_name: "portrait.jpg".to_string(),
        asset_type: "IMAGE".to_string(),
        is_favorite: false,
        deleted_at: None,
        file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
        local_date_time: None,
        duration: None,
        width: Some(4000),
        height: Some(3000),
    };
    mgr.handle_asset(asset).await.unwrap();

    // Create the person.
    db.upsert_person("person-001", "Alice", None, false, false, None, None)
        .await
        .unwrap();

    let face = SyncAssetFaceV1 {
        id: "face-001".to_string(),
        asset_id: "face-asset".to_string(),
        person_id: Some("person-001".to_string()),
        image_width: 4000,
        image_height: 3000,
        bounding_box_x1: 100,
        bounding_box_y1: 200,
        bounding_box_x2: 300,
        bounding_box_y2: 400,
        source_type: Some("MachineLearning".to_string()),
    };

    mgr.handle_asset_face(face).await.unwrap();

    // Verify face count on person was updated.
    let people = db.list_people(false, false).await.unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].face_count, 1);
}

// ── handle_album_asset tests ────────────────────────────────────────

#[tokio::test]
async fn handle_album_asset_links_media_to_album() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    // Create album and asset.
    let album = SyncAlbumV1 {
        id: "link-album".to_string(),
        name: "Linked".to_string(),
        created_at: "2024-01-01T00:00:00.000Z".to_string(),
        updated_at: "2024-01-01T00:00:00.000Z".to_string(),
    };
    mgr.handle_album(album).await.unwrap();

    let asset = SyncAssetV1 {
        id: "link-asset".to_string(),
        original_file_name: "linked.jpg".to_string(),
        asset_type: "IMAGE".to_string(),
        is_favorite: false,
        deleted_at: None,
        file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
        local_date_time: None,
        duration: None,
        width: None,
        height: None,
    };
    mgr.handle_asset(asset).await.unwrap();

    let assoc = SyncAlbumToAssetV1 {
        album_id: "link-album".to_string(),
        asset_id: "link-asset".to_string(),
    };
    mgr.handle_album_asset(assoc).await.unwrap();

    let aid = AlbumId::from_raw("link-album".to_string());
    let items = db.list_album_media(&aid, None, 50).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id.as_str(), "link-asset");
}

// ── deserialize_entity tests ──────────────────────────────────────

#[test]
fn deserialize_entity_valid_asset() {
    let json = serde_json::json!({
        "id": "a1",
        "originalFileName": "photo.jpg",
        "type": "IMAGE",
        "isFavorite": false,
        "deletedAt": null,
        "fileCreatedAt": "2024-01-01T00:00:00.000Z",
        "localDateTime": null,
        "duration": null,
        "exifImageWidth": 100,
        "exifImageHeight": 100,
    });
    let result: Result<SyncAssetV1, _> = deserialize_entity(&json, "AssetV1", 1);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().id, "a1");
}

#[test]
fn deserialize_entity_invalid_returns_error() {
    let json = serde_json::json!({"unexpected": true});
    let result: Result<SyncAssetV1, _> = deserialize_entity(&json, "AssetV1", 42);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("AssetV1"),
        "error should name the entity type: {err}"
    );
    assert!(
        err.contains("42"),
        "error should include line number: {err}"
    );
}

// ── process_entity tests ───────────────────────────────────────────

#[tokio::test]
async fn process_entity_success_pushes_ack_and_increments_counter() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    let mut acks = Vec::new();
    let mut success = 0usize;
    let mut errors = 0usize;

    mgr.process_entity(
        "TestEntity",
        "test-id",
        "cycle-1",
        "upsert",
        "ack-token-1".to_string(),
        async { Ok(()) },
        &mut acks,
        &mut success,
        &mut errors,
    )
    .await;

    assert_eq!(acks, vec!["ack-token-1"]);
    assert_eq!(success, 1);
    assert_eq!(errors, 0);
}

#[tokio::test]
async fn process_entity_failure_increments_error_and_skips_ack() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    let mut acks = Vec::new();
    let mut success = 0usize;
    let mut errors = 0usize;

    mgr.process_entity(
        "TestEntity",
        "test-id",
        "cycle-1",
        "upsert",
        "ack-token-1".to_string(),
        async { Err(LibraryError::Immich("simulated failure".into())) },
        &mut acks,
        &mut success,
        &mut errors,
    )
    .await;

    assert!(acks.is_empty(), "failed entity should not be acked");
    assert_eq!(success, 0);
    assert_eq!(errors, 1);
}

#[tokio::test]
async fn process_entity_records_upsert_audit_action() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    let mut acks = Vec::new();
    let mut success = 0usize;
    let mut errors = 0usize;

    mgr.process_entity(
        "AssetV1",
        "audit-test-upsert",
        "cycle-audit",
        "upsert",
        "ack-u".to_string(),
        async { Ok(()) },
        &mut acks,
        &mut success,
        &mut errors,
    )
    .await;

    let (action, _) = get_audit_record(&db, "audit-test-upsert").await.unwrap();
    assert_eq!(action, "upsert");
}

#[tokio::test]
async fn process_entity_records_delete_audit_action() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    let mut acks = Vec::new();
    let mut success = 0usize;
    let mut errors = 0usize;

    mgr.process_entity(
        "AssetDeleteV1",
        "audit-test-delete",
        "cycle-audit",
        "delete",
        "ack-d".to_string(),
        async { Ok(()) },
        &mut acks,
        &mut success,
        &mut errors,
    )
    .await;

    let (action, _) = get_audit_record(&db, "audit-test-delete").await.unwrap();
    assert_eq!(action, "delete");
}

#[tokio::test]
async fn process_entity_records_error_audit_on_failure() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    let mut acks = Vec::new();
    let mut success = 0usize;
    let mut errors = 0usize;

    mgr.process_entity(
        "AssetV1",
        "audit-test-error",
        "cycle-audit",
        "upsert",
        "ack-e".to_string(),
        async { Err(LibraryError::Immich("boom".into())) },
        &mut acks,
        &mut success,
        &mut errors,
    )
    .await;

    let (action, error_msg) = get_audit_record(&db, "audit-test-error").await.unwrap();
    assert_eq!(action, "error");
    assert!(error_msg.as_deref().unwrap().contains("boom"));
}

// ── handle_sync_reset tests ────────────────────────────────────────

#[tokio::test]
async fn handle_sync_reset_clears_faces_people_checkpoints() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    // Insert an asset, person, face, and checkpoint.
    let asset = SyncAssetV1 {
        id: "reset-asset".to_string(),
        original_file_name: "r.jpg".to_string(),
        asset_type: "IMAGE".to_string(),
        is_favorite: false,
        deleted_at: None,
        file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
        local_date_time: None,
        duration: None,
        width: None,
        height: None,
    };
    mgr.handle_asset(asset).await.unwrap();

    db.upsert_person("p1", "Alice", None, false, false, None, None)
        .await
        .unwrap();
    db.save_sync_checkpoints(&[("AssetV1".to_string(), "ack-1".to_string())])
        .await
        .unwrap();

    let mut is_reset = false;
    let mut existing_ids = None;

    mgr.handle_sync_reset(&mut is_reset, &mut existing_ids)
        .await
        .unwrap();

    assert!(is_reset);
    assert!(existing_ids.is_some());
    let ids = existing_ids.unwrap();
    assert!(
        ids.contains("reset-asset"),
        "existing_ids should contain the asset"
    );

    // People and checkpoints should be cleared.
    let people = db.list_people(false, false).await.unwrap();
    assert!(people.is_empty(), "people should be cleared after reset");
}

#[tokio::test]
async fn finish_sync_emits_complete_event() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, events) = test_sync_manager(db.clone()).await;

    let counters = SyncCounters {
        assets: 5,
        people: 2,
        faces: 3,
        errors: 1,
        ..Default::default()
    };

    // finish_sync with no acks to flush (avoids HTTP call).
    mgr.finish_sync(false, None, &mut Vec::new(), &counters)
        .await
        .unwrap();

    let event = events.try_recv().unwrap();
    match event {
        LibraryEvent::SyncComplete {
            assets,
            people,
            faces,
            errors,
        } => {
            assert_eq!(assets, 5);
            assert_eq!(people, 2);
            assert_eq!(faces, 3);
            assert_eq!(errors, 1);
        }
        other => panic!("expected SyncComplete, got {other:?}"),
    }
}

#[tokio::test]
async fn finish_sync_emits_people_event_when_faces_synced() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, events) = test_sync_manager(db.clone()).await;

    let counters = SyncCounters {
        faces: 1,
        ..Default::default()
    };
    mgr.finish_sync(false, None, &mut Vec::new(), &counters)
        .await
        .unwrap();

    let _ = events.try_recv(); // SyncComplete
    let event = events.try_recv().unwrap();
    assert!(matches!(event, LibraryEvent::PeopleSyncComplete));
}

#[tokio::test]
async fn finish_sync_no_people_event_when_no_faces_or_people() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, events) = test_sync_manager(db.clone()).await;

    let counters = SyncCounters {
        assets: 3,
        ..Default::default()
    };
    mgr.finish_sync(false, None, &mut Vec::new(), &counters)
        .await
        .unwrap();

    let _ = events.try_recv(); // SyncComplete
    assert!(
        events.try_recv().is_err(),
        "no PeopleSyncComplete should be emitted"
    );
}

#[tokio::test]
async fn finish_sync_deletes_orphaned_assets_on_reset() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, _events) = test_sync_manager(db.clone()).await;

    // Insert an asset that will be "orphaned" (not seen during sync).
    let asset = SyncAssetV1 {
        id: "orphan-asset".to_string(),
        original_file_name: "orphan.jpg".to_string(),
        asset_type: "IMAGE".to_string(),
        is_favorite: false,
        deleted_at: None,
        file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
        local_date_time: None,
        duration: None,
        width: None,
        height: None,
    };
    mgr.handle_asset(asset).await.unwrap();

    let id = MediaId::new("orphan-asset".to_string());
    assert!(db.media_exists(&id).await.unwrap());

    let mut orphaned = HashSet::new();
    orphaned.insert("orphan-asset".to_string());

    mgr.finish_sync(
        true,
        Some(orphaned),
        &mut Vec::new(),
        &SyncCounters::default(),
    )
    .await
    .unwrap();

    assert!(
        !db.media_exists(&id).await.unwrap(),
        "orphaned asset should be deleted"
    );
}

// ── handle_asset_delete tests ───────────────────────────────────────

#[tokio::test]
async fn handle_asset_delete_removes_asset() {
    let dir = tempdir().unwrap();
    let db = open_test_db(dir.path()).await;
    let (mgr, events) = test_sync_manager(db.clone()).await;

    // Create then delete.
    let asset = SyncAssetV1 {
        id: "del-asset".to_string(),
        original_file_name: "gone.jpg".to_string(),
        asset_type: "IMAGE".to_string(),
        is_favorite: false,
        deleted_at: None,
        file_created_at: Some("2024-01-01T00:00:00.000Z".to_string()),
        local_date_time: None,
        duration: None,
        width: None,
        height: None,
    };
    mgr.handle_asset(asset).await.unwrap();
    // Drain the AssetSynced event.
    let _ = events.try_recv();

    let id = MediaId::new("del-asset".to_string());
    assert!(db.media_exists(&id).await.unwrap());

    mgr.handle_asset_delete("del-asset").await.unwrap();
    assert!(!db.media_exists(&id).await.unwrap());

    let event = events.try_recv().unwrap();
    assert!(matches!(event, LibraryEvent::AssetDeletedRemote { .. }));
}

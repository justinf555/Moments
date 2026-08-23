// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sync protocol deserialization types for the Immich `/sync/stream` endpoint.
//!
//! All types are newline-delimited JSON (NDJSON) sent by the server.

use serde::{Deserialize, Serialize};
use tracing::error;

use crate::library::error::LibraryError;

// ── Sync protocol types ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct SyncLine {
    #[serde(rename = "type")]
    pub entity_type: String,
    pub data: serde_json::Value,
    pub ack: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SyncStreamRequest {
    pub types: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SyncAckRequest {
    pub acks: Vec<String>,
}

// ── Asset types ─────────────────────────────────────────────────────────────

/// Asset entity emitted on the `AssetsV2` sync stream.
///
/// Identical to the retired `SyncAssetV1` except for `duration`, which
/// Immich changed from a formatted `"H:MM:SS.ffffff"` string to plain
/// integer milliseconds when `asset.duration` became an `integer`
/// column server-side. Issue #679.
#[derive(Debug, Deserialize)]
pub(crate) struct SyncAssetV2 {
    pub id: String,
    #[serde(rename = "originalFileName")]
    pub original_file_name: String,
    #[serde(rename = "fileCreatedAt")]
    pub file_created_at: Option<String>,
    #[serde(rename = "localDateTime")]
    pub local_date_time: Option<String>,
    #[serde(rename = "type")]
    pub asset_type: String,
    #[serde(rename = "deletedAt")]
    pub deleted_at: Option<String>,
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// Runtime in milliseconds, or `None` for stills. Already in the
    /// unit `media.duration_ms` stores, so no conversion is needed —
    /// `AssetsV1` sent this as a `"0:01:30.000000"` string instead.
    pub duration: Option<i64>,
    /// SHA-1 of the file's bytes, base64-encoded. Stored verbatim into
    /// `media.content_hash` so locally-imported and server-pulled rows
    /// share a comparable dedup key. Optional in the serde shape so
    /// older Immich versions (pre-checksum-on-stream) don't break
    /// deserialisation; when absent, sync rows still work but won't
    /// participate in cross-origin dedup.
    pub checksum: Option<String>,
    /// Identifier of the stack this asset belongs to, or `None` when
    /// not stacked. The matching `SyncStackV1` event carries the
    /// primary asset id and arrives independently in the stream — we
    /// just record the pointer here. Immich re-emits the asset
    /// whenever the stack relationship changes. Issue #224.
    #[serde(rename = "stackId", default)]
    pub stack_id: Option<String>,
    /// Whether the asset has any geometric edits applied via
    /// `PUT /assets/{id}/edits`. Pixel-adjustment edits surface as a
    /// separate stacked rendered child rather than via this flag.
    /// Issue #224. Optional for forward/backward compat with servers
    /// that pre-date the edits API.
    ///
    /// Consumed in Phase D (recovery): on editor open we use this in
    /// combination with the `moments-edit` tag to detect "stack with a
    /// Moments-rendered child but no local edits row" and rebuild the
    /// edit state from XMP.
    #[allow(dead_code)] // wired into Phase A for the contract; consumed in Phase D
    #[serde(rename = "isEdited", default)]
    pub is_edited: Option<bool>,
}

/// Stack entity emitted on the `StacksV1` sync stream. Carries the
/// primary asset id for stacks the local library has subscribed to.
/// Issue #224.
#[derive(Debug, Deserialize)]
pub(crate) struct SyncStackV1 {
    pub id: String,
    #[serde(rename = "primaryAssetId")]
    pub primary_asset_id: String,
}

/// Stack-deletion entity. Emitted when a stack is removed server-side
/// (typically because the user un-stacked the assets, or the count
/// dropped below 2 and Immich auto-collapsed it). Issue #224.
#[derive(Debug, Deserialize)]
pub(crate) struct SyncStackDeleteV1 {
    #[serde(rename = "stackId")]
    pub stack_id: String,
}

/// One geometric edit action emitted by `/sync/stream` as
/// `SyncAssetEditV1`. The `parameters` payload is the same shape as
/// the body of `PUT /assets/{id}/edits`, so we keep it as raw JSON
/// here and round-trip it through `ImmichEditAction` deserialization
/// in the handler. Issue #224 Phase B.
#[derive(Debug, Deserialize)]
pub(crate) struct SyncAssetEditV1 {
    pub id: String,
    #[serde(rename = "assetId")]
    pub asset_id: String,
    pub action: String,
    pub parameters: serde_json::Value,
    pub sequence: i64,
}

/// Single-action revert. Carries only the edit id; the local pull
/// handler looks up the parent media before deleting so it can
/// recompose the surviving `EditState`.
#[derive(Debug, Deserialize)]
pub(crate) struct SyncAssetEditDeleteV1 {
    #[serde(rename = "editId")]
    pub edit_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncAssetDeleteV1 {
    #[serde(rename = "assetId")]
    pub asset_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncAssetExifV1 {
    #[serde(rename = "assetId")]
    pub asset_id: String,
    pub make: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "lensModel")]
    pub lens_model: Option<String>,
    #[serde(rename = "fNumber")]
    pub f_number: Option<f32>,
    #[serde(rename = "exposureTime")]
    pub exposure_time: Option<String>,
    pub iso: Option<i64>,
    #[serde(rename = "focalLength")]
    pub focal_length: Option<f32>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    #[serde(rename = "profileDescription")]
    pub profile_description: Option<String>,
}

// ── Album types ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct SyncAlbumV1 {
    pub id: String,
    pub name: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncAlbumDeleteV1 {
    #[serde(rename = "albumId")]
    pub album_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncAlbumToAssetV1 {
    #[serde(rename = "albumId")]
    pub album_id: String,
    #[serde(rename = "assetId")]
    pub asset_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncAlbumToAssetDeleteV1 {
    #[serde(rename = "albumId")]
    pub album_id: String,
    #[serde(rename = "assetId")]
    pub asset_id: String,
}

// ── People types ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct SyncPersonV1 {
    pub id: String,
    pub name: String,
    #[serde(rename = "birthDate")]
    pub birth_date: Option<String>,
    #[serde(rename = "isHidden")]
    pub is_hidden: bool,
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    pub color: Option<String>,
    #[serde(rename = "faceAssetId")]
    pub face_asset_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncPersonDeleteV1 {
    #[serde(rename = "personId")]
    pub person_id: String,
}

/// Face entity emitted on the `AssetFacesV2` sync stream.
///
/// A superset of the retired `SyncAssetFaceV1`: same geometry, plus
/// `deletedAt` and `isVisible`. Issue #679.
#[derive(Debug, Deserialize)]
pub(crate) struct SyncAssetFaceV2 {
    pub id: String,
    #[serde(rename = "assetId")]
    pub asset_id: String,
    #[serde(rename = "personId")]
    pub person_id: Option<String>,
    #[serde(rename = "imageWidth")]
    pub image_width: i32,
    #[serde(rename = "imageHeight")]
    pub image_height: i32,
    #[serde(rename = "boundingBoxX1")]
    pub bounding_box_x1: i32,
    #[serde(rename = "boundingBoxY1")]
    pub bounding_box_y1: i32,
    #[serde(rename = "boundingBoxX2")]
    pub bounding_box_x2: i32,
    #[serde(rename = "boundingBoxY2")]
    pub bounding_box_y2: i32,
    #[serde(rename = "sourceType")]
    pub source_type: Option<String>,
    /// Server-side soft-deletion timestamp, or `None` while the face is
    /// live. Hard deletes still arrive separately as
    /// `AssetFaceDeleteV1`, so this is purely the soft-delete state.
    ///
    /// Issue #680: persisted to `asset_faces.deleted_at`; soft-deleted
    /// faces are filtered out of person queries rather than deleted, so
    /// an un-delete on the server needs no resync.
    #[serde(rename = "deletedAt", default)]
    pub deleted_at: Option<String>,
    /// Whether the face is visible in the asset. Defaults to `true` so
    /// a server that pre-dates the field doesn't hide every face.
    ///
    /// Issue #680: persisted to `asset_faces.is_visible`.
    #[serde(rename = "isVisible", default = "default_true")]
    pub is_visible: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncAssetFaceDeleteV1 {
    #[serde(rename = "assetFaceId")]
    pub asset_face_id: String,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Deserialize a sync entity from its JSON data payload.
pub(crate) fn deserialize_entity<T: serde::de::DeserializeOwned>(
    data: &serde_json::Value,
    entity_type: &str,
    line_number: usize,
) -> Result<T, LibraryError> {
    serde_json::from_value(data.clone()).map_err(|e| {
        let preview: String = data.to_string().chars().take(300).collect();
        error!(
            line_number,
            data = %preview,
            "failed to deserialize {entity_type}: {e}"
        );
        LibraryError::Immich(format!("invalid {entity_type} at line {line_number}: {e}"))
    })
}

/// Parse an ISO 8601 datetime string to Unix timestamp.
pub(crate) fn parse_datetime(s: &Option<String>) -> Option<i64> {
    s.as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_datetime_valid_rfc3339() {
        let s = Some("2024-06-15T10:30:00.000Z".to_string());
        let ts = parse_datetime(&s).unwrap();
        // 2024-06-15 10:30:00 UTC
        assert_eq!(ts, 1_718_447_400);
    }

    #[test]
    fn parse_datetime_none_returns_none() {
        assert!(parse_datetime(&None).is_none());
    }

    #[test]
    fn parse_datetime_invalid_returns_none() {
        let s = Some("not-a-date".to_string());
        assert!(parse_datetime(&s).is_none());
    }

    #[test]
    fn deserialize_sync_line() {
        let json = r#"{"type":"AssetV2","data":{},"ack":"abc123"}"#;
        let line: SyncLine = serde_json::from_str(json).unwrap();
        assert_eq!(line.entity_type, "AssetV2");
        assert_eq!(line.ack, "abc123");
    }

    // ── Additional DTO deserialization tests ──────────────────────────

    #[test]
    fn parse_datetime_with_offset() {
        let s = Some("2024-01-01T00:00:00+05:30".to_string());
        let ts = parse_datetime(&s).unwrap();
        // 2024-01-01T00:00:00+05:30 = 2023-12-31T18:30:00Z
        assert_eq!(ts, 1_704_047_400);
    }

    #[test]
    fn deserialize_sync_asset_v2() {
        let json = serde_json::json!({
            "id": "uuid-1234",
            "originalFileName": "DSC_0001.jpg",
            "fileCreatedAt": "2024-06-15T10:30:00.000Z",
            "localDateTime": "2024-06-15T12:30:00.000Z",
            "type": "IMAGE",
            "deletedAt": null,
            "isFavorite": true,
            "width": 4032,
            "height": 3024,
            "duration": null,
            "checksum": "qZk+NkcGgWq6PiVxeFDCbJzQ2J0="
        });

        let asset: SyncAssetV2 = serde_json::from_value(json).unwrap();
        assert_eq!(asset.id, "uuid-1234");
        assert_eq!(asset.original_file_name, "DSC_0001.jpg");
        assert_eq!(asset.asset_type, "IMAGE");
        assert!(asset.is_favorite);
        assert!(asset.deleted_at.is_none());
        assert_eq!(asset.width, Some(4032));
        assert_eq!(asset.height, Some(3024));
        assert!(asset.duration.is_none());
        assert_eq!(
            asset.checksum.as_deref(),
            Some("qZk+NkcGgWq6PiVxeFDCbJzQ2J0=")
        );
    }

    /// Older Immich versions (pre-checksum-on-stream) don't emit the
    /// `checksum` field — deserialisation must still succeed and leave
    /// the field as `None` rather than failing the whole sync line.
    #[test]
    fn deserialize_sync_asset_v2_without_checksum_is_optional() {
        let json = serde_json::json!({
            "id": "uuid-noosum",
            "originalFileName": "old.jpg",
            "fileCreatedAt": "2024-01-01T00:00:00.000Z",
            "localDateTime": null,
            "type": "IMAGE",
            "deletedAt": null,
            "isFavorite": false,
            "width": null,
            "height": null,
            "duration": null
        });
        let asset: SyncAssetV2 = serde_json::from_value(json).unwrap();
        assert!(asset.checksum.is_none());
    }

    /// Issue #679: `AssetsV2` sends `duration` as integer milliseconds
    /// rather than the `"0:01:30.000000"` string `AssetsV1` used.
    #[test]
    fn deserialize_sync_asset_v2_duration_is_integer_milliseconds() {
        let json = serde_json::json!({
            "id": "uuid-video",
            "originalFileName": "clip.mp4",
            "fileCreatedAt": "2024-06-15T10:30:00.000Z",
            "localDateTime": null,
            "type": "VIDEO",
            "deletedAt": null,
            "isFavorite": false,
            "width": 1920,
            "height": 1080,
            "duration": 3_723_500
        });

        let asset: SyncAssetV2 = serde_json::from_value(json).unwrap();
        assert_eq!(asset.duration, Some(3_723_500));
    }

    /// Issue #224: `AssetV2` carries flat `stackId` (not a nested
    /// object). The matching `SyncStackV1` event carries the primary.
    #[test]
    fn deserialize_sync_asset_v2_with_stack_id() {
        let json = serde_json::json!({
            "id": "asset-uuid",
            "originalFileName": "burst.jpg",
            "fileCreatedAt": "2024-06-15T10:30:00.000Z",
            "localDateTime": null,
            "type": "IMAGE",
            "deletedAt": null,
            "isFavorite": false,
            "width": null,
            "height": null,
            "duration": null,
            "stackId": "stack-uuid",
            "isEdited": true
        });
        let asset: SyncAssetV2 = serde_json::from_value(json).unwrap();
        assert_eq!(asset.stack_id.as_deref(), Some("stack-uuid"));
        assert_eq!(asset.is_edited, Some(true));
    }

    /// `stackId` is `null` when the asset is not in a stack.
    #[test]
    fn deserialize_sync_asset_v2_with_null_stack_id() {
        let json = serde_json::json!({
            "id": "asset-uuid",
            "originalFileName": "lone.jpg",
            "fileCreatedAt": "2024-06-15T10:30:00.000Z",
            "localDateTime": null,
            "type": "IMAGE",
            "deletedAt": null,
            "isFavorite": false,
            "width": null,
            "height": null,
            "duration": null,
            "stackId": null,
            "isEdited": false
        });
        let asset: SyncAssetV2 = serde_json::from_value(json).unwrap();
        assert!(asset.stack_id.is_none());
        assert_eq!(asset.is_edited, Some(false));
    }

    /// Older Immich versions don't emit `stackId`/`isEdited` at all —
    /// deserialisation must succeed and leave both fields `None`.
    #[test]
    fn deserialize_sync_asset_v2_without_stack_fields_is_optional() {
        let json = serde_json::json!({
            "id": "asset-old",
            "originalFileName": "old.jpg",
            "fileCreatedAt": "2024-01-01T00:00:00.000Z",
            "localDateTime": null,
            "type": "IMAGE",
            "deletedAt": null,
            "isFavorite": false,
            "width": null,
            "height": null,
            "duration": null
        });
        let asset: SyncAssetV2 = serde_json::from_value(json).unwrap();
        assert!(asset.stack_id.is_none());
        assert!(asset.is_edited.is_none());
    }

    /// Issue #224: `StackV1` events carry the stack's primary asset id.
    #[test]
    fn deserialize_sync_stack_v1() {
        let json = serde_json::json!({
            "id": "stack-uuid",
            "primaryAssetId": "primary-uuid",
            "ownerId": "owner-uuid",
            "createdAt": "2024-01-01T00:00:00.000Z",
            "updatedAt": "2024-01-02T00:00:00.000Z"
        });
        let stack: SyncStackV1 = serde_json::from_value(json).unwrap();
        assert_eq!(stack.id, "stack-uuid");
        assert_eq!(stack.primary_asset_id, "primary-uuid");
    }

    /// `StackDeleteV1` carries only the stack id.
    #[test]
    fn deserialize_sync_stack_delete_v1() {
        let json = serde_json::json!({ "stackId": "stack-uuid" });
        let del: SyncStackDeleteV1 = serde_json::from_value(json).unwrap();
        assert_eq!(del.stack_id, "stack-uuid");
    }

    #[test]
    fn deserialize_sync_asset_v2_video() {
        let json = serde_json::json!({
            "id": "video-uuid",
            "originalFileName": "MOV_001.mp4",
            "fileCreatedAt": "2024-01-01T00:00:00.000Z",
            "localDateTime": null,
            "type": "VIDEO",
            "deletedAt": null,
            "isFavorite": false,
            "width": 1920,
            "height": 1080,
            "duration": 90_000
        });

        let asset: SyncAssetV2 = serde_json::from_value(json).unwrap();
        assert_eq!(asset.asset_type, "VIDEO");
        assert_eq!(asset.duration, Some(90_000));
    }

    #[test]
    fn deserialize_sync_asset_v2_trashed() {
        let json = serde_json::json!({
            "id": "trashed-uuid",
            "originalFileName": "photo.jpg",
            "fileCreatedAt": "2024-01-01T00:00:00.000Z",
            "localDateTime": null,
            "type": "IMAGE",
            "deletedAt": "2024-06-20T08:00:00.000Z",
            "isFavorite": false,
            "width": null,
            "height": null,
            "duration": null
        });

        let asset: SyncAssetV2 = serde_json::from_value(json).unwrap();
        assert!(asset.deleted_at.is_some());
        assert_eq!(
            asset.deleted_at.as_deref(),
            Some("2024-06-20T08:00:00.000Z")
        );
    }

    #[test]
    fn deserialize_sync_asset_delete_v1() {
        let json = serde_json::json!({ "assetId": "del-uuid-1" });
        let del: SyncAssetDeleteV1 = serde_json::from_value(json).unwrap();
        assert_eq!(del.asset_id, "del-uuid-1");
    }

    #[test]
    fn deserialize_sync_asset_exif_v1_full() {
        let json = serde_json::json!({
            "assetId": "exif-uuid",
            "make": "Canon",
            "model": "EOS R5",
            "lensModel": "RF 24-70mm F2.8L",
            "fNumber": 2.8,
            "exposureTime": "1/250",
            "iso": 400,
            "focalLength": 50.0,
            "latitude": 37.7749,
            "longitude": -122.4194,
            "profileDescription": "sRGB"
        });

        let exif: SyncAssetExifV1 = serde_json::from_value(json).unwrap();
        assert_eq!(exif.asset_id, "exif-uuid");
        assert_eq!(exif.make.as_deref(), Some("Canon"));
        assert_eq!(exif.model.as_deref(), Some("EOS R5"));
        assert_eq!(exif.lens_model.as_deref(), Some("RF 24-70mm F2.8L"));
        assert!((exif.f_number.unwrap() - 2.8).abs() < 0.01);
        assert_eq!(exif.exposure_time.as_deref(), Some("1/250"));
        assert_eq!(exif.iso, Some(400));
        assert!((exif.focal_length.unwrap() - 50.0).abs() < 0.01);
        assert!((exif.latitude.unwrap() - 37.7749).abs() < 0.0001);
        assert!((exif.longitude.unwrap() - (-122.4194)).abs() < 0.0001);
        assert_eq!(exif.profile_description.as_deref(), Some("sRGB"));
    }

    #[test]
    fn deserialize_sync_asset_exif_v1_minimal() {
        let json = serde_json::json!({
            "assetId": "minimal-exif",
            "make": null,
            "model": null,
            "lensModel": null,
            "fNumber": null,
            "exposureTime": null,
            "iso": null,
            "focalLength": null,
            "latitude": null,
            "longitude": null,
            "profileDescription": null
        });

        let exif: SyncAssetExifV1 = serde_json::from_value(json).unwrap();
        assert_eq!(exif.asset_id, "minimal-exif");
        assert!(exif.make.is_none());
        assert!(exif.model.is_none());
        assert!(exif.iso.is_none());
    }

    #[test]
    fn deserialize_sync_album_v1() {
        let json = serde_json::json!({
            "id": "album-uuid",
            "name": "Summer 2024",
            "createdAt": "2024-06-01T00:00:00.000Z",
            "updatedAt": "2024-06-15T12:00:00.000Z"
        });

        let album: SyncAlbumV1 = serde_json::from_value(json).unwrap();
        assert_eq!(album.id, "album-uuid");
        assert_eq!(album.name, "Summer 2024");
        assert_eq!(album.created_at, "2024-06-01T00:00:00.000Z");
        assert_eq!(album.updated_at, "2024-06-15T12:00:00.000Z");
    }

    #[test]
    fn deserialize_sync_album_delete_v1() {
        let json = serde_json::json!({ "albumId": "album-del-1" });
        let del: SyncAlbumDeleteV1 = serde_json::from_value(json).unwrap();
        assert_eq!(del.album_id, "album-del-1");
    }

    #[test]
    fn deserialize_sync_album_to_asset_v1() {
        let json = serde_json::json!({
            "albumId": "album-1",
            "assetId": "asset-1"
        });

        let assoc: SyncAlbumToAssetV1 = serde_json::from_value(json).unwrap();
        assert_eq!(assoc.album_id, "album-1");
        assert_eq!(assoc.asset_id, "asset-1");
    }

    #[test]
    fn deserialize_sync_album_to_asset_delete_v1() {
        let json = serde_json::json!({
            "albumId": "album-2",
            "assetId": "asset-2"
        });

        let del: SyncAlbumToAssetDeleteV1 = serde_json::from_value(json).unwrap();
        assert_eq!(del.album_id, "album-2");
        assert_eq!(del.asset_id, "asset-2");
    }

    #[test]
    fn deserialize_sync_person_v1() {
        let json = serde_json::json!({
            "id": "person-uuid",
            "name": "Alice Smith",
            "birthDate": "1990-05-15",
            "isHidden": false,
            "isFavorite": true,
            "color": "#FF5733",
            "faceAssetId": "face-asset-uuid"
        });

        let person: SyncPersonV1 = serde_json::from_value(json).unwrap();
        assert_eq!(person.id, "person-uuid");
        assert_eq!(person.name, "Alice Smith");
        assert_eq!(person.birth_date.as_deref(), Some("1990-05-15"));
        assert!(!person.is_hidden);
        assert!(person.is_favorite);
        assert_eq!(person.color.as_deref(), Some("#FF5733"));
        assert_eq!(person.face_asset_id.as_deref(), Some("face-asset-uuid"));
    }

    #[test]
    fn deserialize_sync_person_v1_minimal() {
        let json = serde_json::json!({
            "id": "person-min",
            "name": "",
            "birthDate": null,
            "isHidden": true,
            "isFavorite": false,
            "color": null,
            "faceAssetId": null
        });

        let person: SyncPersonV1 = serde_json::from_value(json).unwrap();
        assert_eq!(person.name, "");
        assert!(person.is_hidden);
        assert!(person.birth_date.is_none());
        assert!(person.face_asset_id.is_none());
    }

    #[test]
    fn deserialize_sync_person_delete_v1() {
        let json = serde_json::json!({ "personId": "person-del-1" });
        let del: SyncPersonDeleteV1 = serde_json::from_value(json).unwrap();
        assert_eq!(del.person_id, "person-del-1");
    }

    #[test]
    fn deserialize_sync_asset_face_v2() {
        let json = serde_json::json!({
            "id": "face-uuid",
            "assetId": "asset-uuid",
            "personId": "person-uuid",
            "imageWidth": 4032,
            "imageHeight": 3024,
            "boundingBoxX1": 100,
            "boundingBoxY1": 200,
            "boundingBoxX2": 300,
            "boundingBoxY2": 400,
            "sourceType": "MachineLearning"
        });

        let face: SyncAssetFaceV2 = serde_json::from_value(json).unwrap();
        assert_eq!(face.id, "face-uuid");
        assert_eq!(face.asset_id, "asset-uuid");
        assert_eq!(face.person_id.as_deref(), Some("person-uuid"));
        assert_eq!(face.image_width, 4032);
        assert_eq!(face.image_height, 3024);
        assert_eq!(face.bounding_box_x1, 100);
        assert_eq!(face.bounding_box_y1, 200);
        assert_eq!(face.bounding_box_x2, 300);
        assert_eq!(face.bounding_box_y2, 400);
        assert_eq!(face.source_type.as_deref(), Some("MachineLearning"));
    }

    #[test]
    fn deserialize_sync_asset_face_v2_no_person() {
        let json = serde_json::json!({
            "id": "face-no-person",
            "assetId": "asset-uuid",
            "personId": null,
            "imageWidth": 1920,
            "imageHeight": 1080,
            "boundingBoxX1": 50,
            "boundingBoxY1": 50,
            "boundingBoxX2": 150,
            "boundingBoxY2": 150,
            "sourceType": null
        });

        let face: SyncAssetFaceV2 = serde_json::from_value(json).unwrap();
        assert!(face.person_id.is_none());
        assert!(face.source_type.is_none());
    }

    /// Issue #679: `AssetFaceV2` adds `deletedAt` and `isVisible` on top
    /// of the retired V1 shape.
    #[test]
    fn deserialize_sync_asset_face_v2_visibility_fields() {
        let json = serde_json::json!({
            "id": "face-v2",
            "assetId": "asset-uuid",
            "personId": "person-uuid",
            "imageWidth": 4032,
            "imageHeight": 3024,
            "boundingBoxX1": 100,
            "boundingBoxY1": 200,
            "boundingBoxX2": 300,
            "boundingBoxY2": 400,
            "sourceType": "MachineLearning",
            "deletedAt": "2026-08-23T10:15:18.000Z",
            "isVisible": false
        });

        let face: SyncAssetFaceV2 = serde_json::from_value(json).unwrap();
        assert_eq!(face.deleted_at.as_deref(), Some("2026-08-23T10:15:18.000Z"));
        assert!(!face.is_visible);
    }

    /// A server that pre-dates the V2 face fields must not make every
    /// face invisible — `isVisible` defaults to `true` when absent.
    #[test]
    fn deserialize_sync_asset_face_v2_defaults_to_visible() {
        let json = serde_json::json!({
            "id": "face-no-visibility",
            "assetId": "asset-uuid",
            "personId": null,
            "imageWidth": 1920,
            "imageHeight": 1080,
            "boundingBoxX1": 50,
            "boundingBoxY1": 50,
            "boundingBoxX2": 150,
            "boundingBoxY2": 150,
            "sourceType": null
        });

        let face: SyncAssetFaceV2 = serde_json::from_value(json).unwrap();
        assert!(face.is_visible);
        assert!(face.deleted_at.is_none());
    }

    #[test]
    fn deserialize_sync_asset_face_delete_v1() {
        let json = serde_json::json!({ "assetFaceId": "face-del-1" });
        let del: SyncAssetFaceDeleteV1 = serde_json::from_value(json).unwrap();
        assert_eq!(del.asset_face_id, "face-del-1");
    }

    #[test]
    fn deserialize_entity_valid() {
        let data = serde_json::json!({ "assetId": "test-id" });
        let result: Result<SyncAssetDeleteV1, _> = deserialize_entity(&data, "AssetDeleteV1", 1);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().asset_id, "test-id");
    }

    #[test]
    fn deserialize_entity_invalid_returns_error() {
        let data = serde_json::json!({ "wrong_field": "value" });
        let result: Result<SyncAssetDeleteV1, _> = deserialize_entity(&data, "AssetDeleteV1", 42);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("AssetDeleteV1"));
        assert!(err.contains("42"));
    }

    #[test]
    fn sync_stream_request_serializes() {
        let req = SyncStreamRequest {
            types: vec!["AssetsV2".to_string(), "AlbumsV1".to_string()],
        };
        let json = serde_json::to_value(&req).unwrap();
        let types = json["types"].as_array().unwrap();
        assert_eq!(types.len(), 2);
        assert_eq!(types[0], "AssetsV2");
    }

    #[test]
    fn sync_ack_request_serializes() {
        let req = SyncAckRequest {
            acks: vec!["ack1".to_string(), "ack2".to_string()],
        };
        let json = serde_json::to_value(&req).unwrap();
        let acks = json["acks"].as_array().unwrap();
        assert_eq!(acks.len(), 2);
    }

    #[test]
    fn sync_line_preserves_data_payload() {
        let json = r#"{"type":"AssetExifV1","data":{"assetId":"x","make":"Canon"},"ack":"ack1"}"#;
        let line: SyncLine = serde_json::from_str(json).unwrap();
        assert_eq!(line.data["assetId"], "x");
        assert_eq!(line.data["make"], "Canon");
    }
}

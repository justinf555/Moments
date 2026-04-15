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

#[derive(Debug, Deserialize)]
pub(crate) struct SyncAssetV1 {
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
    pub duration: Option<String>,
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

#[derive(Debug, Deserialize)]
pub(crate) struct SyncAssetFaceV1 {
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
        error!(line_number, "failed to deserialize {entity_type}: {e}");
        LibraryError::Immich(format!("invalid {entity_type} at line {line_number}: {e}"))
    })
}

/// Parse an ISO 8601 datetime string to Unix timestamp.
pub(crate) fn parse_datetime(s: &Option<String>) -> Option<i64> {
    s.as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
}

/// Parse Immich duration string (e.g. "0:01:30.000000") to milliseconds.
pub(crate) fn parse_duration_ms(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours: u64 = parts[0].parse().ok()?;
    let minutes: u64 = parts[1].parse().ok()?;
    let seconds: f64 = parts[2].parse().ok()?;
    Some(hours * 3_600_000 + minutes * 60_000 + (seconds * 1000.0) as u64)
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
    fn parse_duration_ms_standard() {
        assert_eq!(parse_duration_ms("0:01:30.000000"), Some(90_000));
    }

    #[test]
    fn parse_duration_ms_with_hours() {
        assert_eq!(parse_duration_ms("1:02:03.500000"), Some(3_723_500));
    }

    #[test]
    fn parse_duration_ms_invalid_format() {
        assert!(parse_duration_ms("invalid").is_none());
    }

    #[test]
    fn deserialize_sync_line() {
        let json = r#"{"type":"AssetV1","data":{},"ack":"abc123"}"#;
        let line: SyncLine = serde_json::from_str(json).unwrap();
        assert_eq!(line.entity_type, "AssetV1");
        assert_eq!(line.ack, "abc123");
    }
}

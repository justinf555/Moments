use async_trait::async_trait;
use tracing::warn;

use crate::library::error::LibraryError;
use crate::library::metadata::MediaMetadataRecord;

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};
use crate::sync::providers::immich::types::*;

pub struct AssetExifHandler;

#[async_trait]
impl SyncEntityHandler for AssetExifHandler {
    fn entity_type(&self) -> &'static str {
        "AssetExifV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let exif: SyncAssetExifV1 = deserialize_entity(data, "AssetExifV1", line_number)?;
        let id = exif.asset_id.clone();

        // Issue #626: `exif.asset_id` is the Immich UUID; `media_metadata.media_id`
        // references the local `MediaId`. Translate via `external_id` lookup;
        // skip with a warning if the parent asset hasn't been processed yet.
        let media_id = match ctx
            .library
            .media()
            .id_by_external_id(&exif.asset_id)
            .await?
        {
            Some(local) => local,
            None => {
                warn!(
                    asset_id = %exif.asset_id,
                    "AssetExifV1: parent asset not found locally; skipping metadata row"
                );
                return Ok(HandlerResult {
                    entity_id: id,
                    audit_action: "upsert",
                    counter: CounterKind::Exifs,
                });
            }
        };

        let record = MediaMetadataRecord {
            media_id,
            camera_make: exif.make,
            camera_model: exif.model,
            lens_model: exif.lens_model,
            aperture: exif.f_number,
            shutter_str: exif.exposure_time,
            iso: exif.iso.map(|v| v as u32),
            focal_length: exif.focal_length,
            gps_lat: exif.latitude,
            gps_lon: exif.longitude,
            gps_alt: None,
            color_space: exif.profile_description,
        };

        ctx.library.metadata().upsert_metadata(&record).await?;

        Ok(HandlerResult {
            entity_id: id,
            audit_action: "upsert",
            counter: CounterKind::Exifs,
        })
    }
}

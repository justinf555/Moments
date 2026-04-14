use std::path::PathBuf;
use std::sync::Arc;

use tracing::info;

use super::db::Database;
use super::error::LibraryError;
use super::format::{FormatRegistry, RawHandler, StandardHandler, VideoHandler};
use super::immich_client::ImmichClient;
use super::importer::collect_candidates;
use super::media::{MediaId, MediaItem, MediaRecord, MediaType};
use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::importer::ImportSummary;

/// Upload job for importing local files to the Immich server.
pub struct ImmichImportJob {
    pub client: ImmichClient,
    pub db: Database,
    pub events: EventSender,
}

impl ImmichImportJob {
    pub async fn run(&self, sources: Vec<PathBuf>) {
        let mut registry = FormatRegistry::new();
        registry.register(Arc::new(StandardHandler));
        registry.register(Arc::new(RawHandler));
        registry.register(Arc::new(VideoHandler));

        let candidates = collect_candidates(sources);
        let total = candidates.len();
        info!(total, "upload candidates collected");

        let mut summary = ImportSummary::default();
        let now = chrono::Utc::now().timestamp();

        // Insert all candidates into the upload queue.
        for path in &candidates {
            let path_str = path.to_string_lossy();
            // Best-effort: queue tracking is advisory, not blocking.
            let _ = self.db.insert_upload_pending(&path_str, now).await;
        }

        for (idx, path) in candidates.iter().enumerate() {
            let current = idx + 1;
            // Receiver may be dropped during shutdown.
            self.events.send(AppEvent::ImportProgress {
                current,
                total,
                imported: summary.imported,
                skipped: summary.skipped_duplicates + summary.skipped_unsupported,
                failed: summary.failed,
            });

            match self.upload_one(&registry, path).await {
                Ok(UploadResult::Created) => summary.imported += 1,
                Ok(UploadResult::Duplicate) => summary.skipped_duplicates += 1,
                Ok(UploadResult::Unsupported) => summary.skipped_unsupported += 1,
                Err(e) => {
                    let path_str = path.to_string_lossy();
                    tracing::warn!(path = %path_str, "upload failed: {e}");
                    // Best-effort: status tracking is advisory.
                    let _ = self
                        .db
                        .set_upload_status(&path_str, 2, Some(&e.to_string()))
                        .await;
                    summary.failed += 1;
                }
            }
        }

        // Best-effort: cleanup failure doesn't affect the import result.
        let _ = self.db.clear_completed_uploads().await;

        // Receiver may be dropped during shutdown.
        self.events.send(AppEvent::ImportComplete { summary });
    }

    async fn upload_one(
        &self,
        formats: &FormatRegistry,
        source: &std::path::Path,
    ) -> Result<UploadResult, LibraryError> {
        use sha1::Digest;

        let path_str = source.to_string_lossy().to_string();

        // Extension/format check.
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if formats.media_type_with_sniff(source, &ext).is_none() {
            // Best-effort: status tracking is advisory.
            let _ = self.db.set_upload_status(&path_str, 3, None).await;
            return Ok(UploadResult::Unsupported);
        }

        // Compute SHA-1 hash (Immich dedup).
        let source_clone = source.to_path_buf();
        let sha1_hex = tokio::task::spawn_blocking(move || -> Result<String, LibraryError> {
            let data = std::fs::read(&source_clone).map_err(LibraryError::Io)?;
            let hash = sha1::Sha1::digest(&data);
            Ok(format!("{:x}", hash))
        })
        .await
        .map_err(|e| LibraryError::Runtime(e.to_string()))??;

        // Best-effort: status tracking is advisory.
        let _ = self.db.set_upload_hash(&path_str, &sha1_hex).await;

        // Get file created time for the upload.
        let meta = std::fs::metadata(source).ok();
        let to_rfc3339 = |t: std::time::SystemTime| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0))
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
        };
        let now_rfc3339 = chrono::Utc::now().to_rfc3339();
        let file_created_at = meta
            .as_ref()
            .and_then(|m| m.created().ok())
            .map(&to_rfc3339)
            .unwrap_or_else(|| now_rfc3339.clone());
        let file_modified_at = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(&to_rfc3339)
            .unwrap_or(now_rfc3339);

        let filename = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload");
        let device_asset_id = format!("{filename}-{sha1_hex}");

        // Upload to Immich.
        let resp = self
            .client
            .upload_asset(
                source,
                &device_asset_id,
                &file_created_at,
                &file_modified_at,
                Some(&sha1_hex),
            )
            .await?;

        if resp.status == "duplicate" {
            // Best-effort: status tracking is advisory.
            let _ = self.db.set_upload_status(&path_str, 3, None).await;
            return Ok(UploadResult::Duplicate);
        }

        // Insert into local cache with server-assigned UUID.
        let media_type = if formats.is_video(&ext) {
            MediaType::Video
        } else {
            MediaType::Image
        };

        let now = chrono::Utc::now().timestamp();
        let record = MediaRecord {
            id: MediaId::new(resp.id.clone()),
            relative_path: format!("immich/{}", resp.id),
            original_filename: filename.to_string(),
            file_size: std::fs::metadata(source)
                .map(|m| m.len() as i64)
                .unwrap_or(0),
            imported_at: now,
            media_type,
            taken_at: None, // Server extracts EXIF
            width: None,
            height: None,
            orientation: 1,
            duration_ms: None,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
        };

        let item = MediaItem {
            id: MediaId::new(resp.id.clone()),
            taken_at: None,
            imported_at: now,
            original_filename: filename.to_string(),
            width: None,
            height: None,
            orientation: 1,
            media_type,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
            duration_ms: None,
        };

        self.db.upsert_media(&record).await?;
        // Receiver may be dropped during shutdown.
        self.events.send(AppEvent::AssetSynced { item });
        // Best-effort: status tracking is advisory.
        let _ = self.db.set_upload_status(&path_str, 1, None).await;

        Ok(UploadResult::Created)
    }
}

enum UploadResult {
    Created,
    Duplicate,
    Unsupported,
}

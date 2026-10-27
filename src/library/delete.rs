// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Permanent deletion: database rows first, then best-effort file cleanup.

use std::path::PathBuf;

use tracing::debug;

use super::error::LibraryError;
use super::media::MediaId;
use super::Library;

impl Library {
    /// Permanently delete assets: DB first, then best-effort file cleanup.
    ///
    /// Collects file paths before the transactional DB delete so that
    /// a DB failure leaves files intact (recoverable orphans are
    /// preferable to references pointing at deleted files).
    pub async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        let original_paths = self.media.collect_original_paths(ids).await;
        self.media.delete_permanently(ids).await?;
        self.cleanup_files(ids, &original_paths).await;
        Ok(())
    }

    /// Permanently delete assets without recording to the outbox.
    ///
    /// Used by pull sync when processing server-driven deletions —
    /// these should not be pushed back to the server.
    pub async fn delete_permanently_from_sync(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        let original_paths = self.media.collect_original_paths(ids).await;
        self.media.delete_permanently_no_record(ids).await?;
        self.cleanup_files(ids, &original_paths).await;
        Ok(())
    }

    /// Best-effort removal of original files and thumbnails from disk.
    pub(super) async fn cleanup_files(
        &self,
        ids: &[MediaId],
        original_paths: &[(MediaId, PathBuf)],
    ) {
        for (id, path) in original_paths {
            if let Err(e) = tokio::fs::remove_file(path).await {
                debug!(id = %id, path = %path.display(), "original not on disk or already removed: {e}");
            }
        }
        for id in ids {
            let thumb = self.thumbnails.thumbnail_path(id);
            if let Err(e) = tokio::fs::remove_file(&thumb).await {
                debug!(id = %id, "thumbnail not on disk or already removed: {e}");
            }
        }
    }
}

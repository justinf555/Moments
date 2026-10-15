// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Queries used only by Immich sync: pull upserts, external-id lookups
//! and the `last_seen_at` heartbeat.

use super::EditingRepository;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

impl EditingRepository {
    /// Get the local id of the rendered sibling for an edit, if any.
    ///
    /// Phase C records this when a pixel-adjustment edit is saved: the
    /// rendered JPEG goes into a new media row and that row's MediaId
    /// is stamped here. `None` means either no edit row, no render
    /// (geometric-only edit), or a previously-rendered row that was
    /// reverted.
    pub async fn server_rendered_asset_id(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaId>, LibraryError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT server_rendered_asset_id FROM edits WHERE media_id = ?")
                .bind(id.as_str())
                .fetch_optional(self.db.pool())
                .await
                .map_err(LibraryError::Db)?;

        Ok(row.and_then(|(s,)| s.map(MediaId::new)))
    }

    /// Stamp (or clear, with `None`) the rendered sibling pointer on
    /// an existing edit row. Called by the Phase C save flow once the
    /// rendered media row has been inserted, and by revert to clear.
    pub async fn set_server_rendered_asset_id(
        &self,
        id: &MediaId,
        rendered: Option<&MediaId>,
    ) -> Result<(), LibraryError> {
        sqlx::query("UPDATE edits SET server_rendered_asset_id = ? WHERE media_id = ?")
            .bind(rendered.map(|r| r.as_str()))
            .bind(id.as_str())
            .execute(self.db.pool())
            .await
            .map_err(LibraryError::Db)?;
        Ok(())
    }
}

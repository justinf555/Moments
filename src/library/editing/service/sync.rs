// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Operations used only by Immich sync: pull upserts (no outbox
//! recording), external-id lookups and the `last_seen_at` heartbeat.

use super::EditingService;
use crate::library::editing::model::EditState;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

impl EditingService {
    // ── Phase C orchestration helpers ─────────────────────────────────
    //
    // These bypass the recorder because the caller — `Library::save_pixel_edit` /
    // `Library::revert_edit` — emits its own mutation sequence
    // (StackCreated + AssetTaggedMomentsEdit, or AssetEditsCleared +
    // the rendered-asset cleanup). Recording AssetEditsApplied here as
    // well would race those.

    pub async fn server_rendered_asset_id(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaId>, LibraryError> {
        self.repo.server_rendered_asset_id(id).await
    }

    pub async fn set_server_rendered_asset_id(
        &self,
        id: &MediaId,
        rendered: Option<&MediaId>,
    ) -> Result<(), LibraryError> {
        self.repo.set_server_rendered_asset_id(id, rendered).await
    }

    pub async fn upsert_edit_state_no_record(
        &self,
        id: &MediaId,
        state: &EditState,
    ) -> Result<(), LibraryError> {
        self.repo.upsert_edit_state(id, state).await
    }

    pub async fn delete_edit_state_no_record(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.delete_edit_state(id).await
    }
}

// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Operations used only by Immich sync: pull upserts (no outbox
//! recording), external-id lookups and the `last_seen_at` heartbeat.

use super::EditingService;
use crate::library::db::Tx;
use crate::library::editing::model::EditState;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;

impl EditingService {
    // ── Phase C orchestration helpers ─────────────────────────────────
    //
    // These bypass the recorder because the caller — `Library::save_pixel_edit` /
    // `Library::revert_edit` — runs them on its own transaction and
    // records its own mutation sequence
    // (StackCreated + AssetTaggedMomentsEdit, or AssetEditsCleared +
    // the rendered-asset cleanup). Recording AssetEditsApplied here as
    // well would race those.

    pub async fn server_rendered_asset_id(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaId>, LibraryError> {
        self.repo.server_rendered_asset_id(id).await
    }

    /// [`Self::server_rendered_asset_id`] on the caller's transaction.
    pub(crate) async fn server_rendered_asset_id_in_tx(
        &self,
        tx: &mut Tx,
        id: &MediaId,
    ) -> Result<Option<MediaId>, LibraryError> {
        self.repo.server_rendered_asset_id_in_tx(tx, id).await
    }

    /// Stamp the rendered sibling pointer on the caller's transaction.
    pub(crate) async fn set_server_rendered_asset_id_in_tx(
        &self,
        tx: &mut Tx,
        id: &MediaId,
        rendered: Option<&MediaId>,
    ) -> Result<(), LibraryError> {
        self.repo
            .set_server_rendered_asset_id_in_tx(tx, id, rendered)
            .await
    }

    /// Upsert the edit state on the caller's transaction, recording
    /// nothing.
    pub(crate) async fn upsert_edit_state_no_record_in_tx(
        &self,
        tx: &mut Tx,
        id: &MediaId,
        state: &EditState,
    ) -> Result<(), LibraryError> {
        self.repo.upsert_edit_state_in_tx(tx, id, state).await
    }

    /// Delete the edit state on the caller's transaction, recording
    /// nothing.
    pub(crate) async fn delete_edit_state_no_record_in_tx(
        &self,
        tx: &mut Tx,
        id: &MediaId,
    ) -> Result<(), LibraryError> {
        self.repo.delete_edit_state_in_tx(tx, id).await
    }
}

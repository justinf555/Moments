// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Operations used only by Immich sync: pull upserts (no outbox
//! recording), external-id lookups and the `last_seen_at` heartbeat.

use super::MetadataService;
use crate::library::error::LibraryError;
use crate::library::metadata::model::MediaMetadataRecord;

impl MetadataService {
    /// Insert or replace metadata from the sync stream.
    pub async fn upsert_metadata(&self, record: &MediaMetadataRecord) -> Result<(), LibraryError> {
        self.repo.upsert(record).await
    }
}

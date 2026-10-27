// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! The app's [`EvictionGuard`]: only originals the Immich server already
//! holds, and that no unfinished outbox entry still needs, may go.

use std::sync::Weak;

use async_trait::async_trait;
use tracing::warn;

use super::cache::EvictionGuard;
use crate::library::media::MediaId;
use crate::library::Library;
use crate::sync::outbox::OutboxRepository;

/// Allows eviction of a cached original when
///
/// 1. its media row has an `external_id`: the server holds the bytes and
///    [`super::CachedResolver`] can download them again, and
/// 2. no pending, failed or dead-letter outbox entry refers to it, so a
///    local import waiting to upload keeps its only copy.
///
/// Holds the library weakly: the library owns the resolver that owns
/// this guard. Any lookup error keeps the file.
pub struct LibraryEvictionGuard {
    library: Weak<Library>,
    outbox: OutboxRepository,
}

impl LibraryEvictionGuard {
    pub fn new(library: Weak<Library>, outbox: OutboxRepository) -> Self {
        Self { library, outbox }
    }
}

#[async_trait]
impl EvictionGuard for LibraryEvictionGuard {
    async fn may_evict(&self, id: &MediaId) -> bool {
        let Some(library) = self.library.upgrade() else {
            return false;
        };
        match library.media().get_media_record(id).await {
            Ok(Some(record)) if record.external_id.is_some() => {}
            // No row (orphan / mid-import) or not on the server yet.
            Ok(_) => return false,
            Err(e) => {
                warn!(%id, "cache eviction: media lookup failed: {e}");
                return false;
            }
        }
        match self.outbox.has_unfinished(id.as_str()).await {
            Ok(unfinished) => !unfinished,
            Err(e) => {
                warn!(%id, "cache eviction: outbox lookup failed: {e}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::library::bundle::Bundle;
    use crate::library::config::{LibraryConfig, LocalStorageMode};
    use crate::library::db::Database;
    use crate::library::media::{sharded_original_relative, MediaRecord};
    use crate::library::mutation::Mutation;
    use crate::library::resolver::LocalResolver;

    async fn open_library(dir: &std::path::Path) -> (Arc<Library>, OutboxRepository) {
        let bundle = Bundle::create(
            &dir.join("Test.library"),
            &LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            },
        )
        .unwrap();
        let originals = bundle.originals.clone();
        let db = Database::new();
        let library = Library::open(
            bundle,
            LocalStorageMode::Managed,
            db.clone(),
            Arc::new(crate::sync::outbox::NoOpRecorder),
            Arc::new(LocalResolver::new(originals, LocalStorageMode::Managed)),
        )
        .await
        .unwrap();
        (Arc::new(library), OutboxRepository::new(db))
    }

    /// A media row at its own sharded path (`relative_path` is unique).
    fn test_record(id: MediaId) -> MediaRecord {
        let mut rec = crate::library::db::test_helpers::test_record(id.clone());
        rec.relative_path = sharded_original_relative(&id);
        rec
    }

    #[tokio::test]
    async fn only_uploaded_assets_without_pending_work_are_evictable() {
        let dir = tempfile::tempdir().unwrap();
        let (library, outbox) = open_library(dir.path()).await;
        let guard = LibraryEvictionGuard::new(Arc::downgrade(&library), outbox.clone());

        // Synced from Immich: evictable.
        let synced = MediaId::new("synced0001".to_string());
        let mut rec = test_record(synced.clone());
        rec.external_id = Some("server-1".to_string());
        library.media().insert_media(&rec).await.unwrap();
        assert!(guard.may_evict(&synced).await);

        // Local import, not uploaded yet: keep.
        let local = MediaId::new("local00001".to_string());
        library
            .media()
            .insert_media(&test_record(local.clone()))
            .await
            .unwrap();
        assert!(!guard.may_evict(&local).await);

        // Has a server id but an unfinished outbox entry: keep.
        let busy = MediaId::new("busy000001".to_string());
        let mut rec = test_record(busy.clone());
        rec.external_id = Some("server-2".to_string());
        library.media().insert_media(&rec).await.unwrap();
        let rows = Mutation::AssetTrashed {
            ids: vec![busy.clone()],
        }
        .to_outbox_rows();
        outbox.insert(&rows[0]).await.unwrap();
        assert!(!guard.may_evict(&busy).await);

        // No media row at all (orphan or mid-import copy): keep.
        assert!(
            !guard
                .may_evict(&MediaId::new("orphan0001".to_string()))
                .await
        );
    }

    #[tokio::test]
    async fn dropped_library_blocks_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let (library, outbox) = open_library(dir.path()).await;
        let guard = LibraryEvictionGuard::new(Arc::downgrade(&library), outbox);
        drop(library);
        assert!(
            !guard
                .may_evict(&MediaId::new("any0000001".to_string()))
                .await
        );
    }
}

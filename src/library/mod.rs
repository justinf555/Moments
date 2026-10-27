// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod album;
pub mod bundle;
pub mod config;
pub mod db;
mod delete;
mod edit_sync;
pub mod editing;
pub mod error;
pub mod event_emitter;
pub mod faces;
pub mod maintenance;
pub mod media;
pub mod metadata;
pub mod mutation;
pub mod purge_trash;
pub mod recorder;
pub mod resolver;
pub mod thumbnail;

use std::sync::Arc;

use tracing::{debug, info, instrument};

use album::AlbumService;
use bundle::Bundle;
use config::LocalStorageMode;
use db::Database;
use editing::EditingService;
use error::LibraryError;
use faces::FacesService;
use maintenance::MaintenanceService;
use media::MediaService;
use metadata::MetadataService;
use recorder::MutationRecorder;
use thumbnail::ThumbnailService;

/// The Moments library — a single concrete type composing all feature services.
///
/// Constructed via [`Library::open`] with a validated [`Bundle`] and
/// [`LocalStorageMode`]. Single-feature operations go through the service
/// accessors (`media()`, `albums()`, `faces()`, …). Operations spanning
/// several services live on `Library` itself, in `delete.rs` and
/// `edit_sync.rs`. The UI reaches all of this through the client layer.
pub struct Library {
    /// Shared database handle, used by cross-service operations that
    /// need one transaction across several services (`edit_sync.rs`).
    db: Database,
    albums: AlbumService,
    faces: FacesService,
    editing: EditingService,
    maintenance: MaintenanceService,
    media: MediaService,
    metadata: MetadataService,
    thumbnails: ThumbnailService,
    /// Same recorder Arc that the per-service constructors received —
    /// kept on `Library` so cross-service operations like
    /// [`Library::save_pixel_edit`] can record their own mutations
    /// (Stack*, *MomentsEdit) without going through a per-service
    /// shim.
    recorder: Arc<dyn MutationRecorder>,
}

impl Library {
    /// Open a library from a validated bundle.
    ///
    /// The `db` handle must have been created with [`Database::new`] — this
    /// method calls [`Database::open`] to connect and run migrations. All
    /// clones of `db` (e.g. held by sync or outbox) become active.
    #[instrument(skip_all, fields(path = %bundle.path.display(), mode = ?mode))]
    pub async fn open(
        bundle: Bundle,
        mode: LocalStorageMode,
        db: Database,
        recorder: Arc<dyn MutationRecorder>,
        resolver: Arc<dyn resolver::OriginalResolver>,
    ) -> Result<Self, LibraryError> {
        info!("opening library");

        let db_path = bundle.database.join("moments.db");
        db.open(&db_path).await?;

        let albums = AlbumService::new(db.clone(), Arc::clone(&recorder));
        let faces = FacesService::new(db.clone(), bundle.thumbnails.clone(), Arc::clone(&recorder));
        let editing = EditingService::new(db.clone(), Arc::clone(&recorder));
        let maintenance =
            MaintenanceService::new(db.clone(), bundle.originals.clone(), mode.clone());
        let media = MediaService::new(
            db.clone(),
            bundle.originals.clone(),
            mode,
            Arc::clone(&recorder),
            resolver,
        );
        let metadata = MetadataService::new(db.clone());
        let thumbnails = ThumbnailService::new(db.clone(), bundle.thumbnails.clone());

        debug!("library ready");
        Ok(Self {
            db,
            albums,
            faces,
            editing,
            maintenance,
            media,
            metadata,
            thumbnails,
            recorder,
        })
    }

    // ── Service accessors ───────────────────────────────────────────

    pub fn media(&self) -> &MediaService {
        &self.media
    }

    pub fn metadata(&self) -> &MetadataService {
        &self.metadata
    }

    pub fn thumbnails(&self) -> &ThumbnailService {
        &self.thumbnails
    }

    pub fn albums(&self) -> &AlbumService {
        &self.albums
    }

    pub fn faces(&self) -> &FacesService {
        &self.faces
    }

    pub fn editing(&self) -> &EditingService {
        &self.editing
    }

    pub fn maintenance(&self) -> &MaintenanceService {
        &self.maintenance
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::config::LibraryConfig;

    pub(super) async fn open_test_library(bundle: Bundle) -> Library {
        let originals = bundle.originals.clone();
        Library::open(
            bundle,
            LocalStorageMode::Managed,
            Database::new(),
            Arc::new(crate::sync::outbox::NoOpRecorder),
            Arc::new(resolver::LocalResolver::new(
                originals,
                LocalStorageMode::Managed,
            )),
        )
        .await
        .unwrap()
    }

    /// Open a library with a CapturingRecorder so tests can assert
    /// the outbox-bound mutation sequence (see `edit_sync` tests).
    pub(super) async fn open_test_library_with_recorder(
        bundle: Bundle,
    ) -> (
        Library,
        Arc<crate::library::recorder::tests::CapturingRecorder>,
    ) {
        let recorder = Arc::new(crate::library::recorder::tests::CapturingRecorder::default());
        let originals = bundle.originals.clone();
        let library = Library::open(
            bundle,
            LocalStorageMode::Managed,
            Database::new(),
            Arc::clone(&recorder) as Arc<dyn MutationRecorder>,
            Arc::new(resolver::LocalResolver::new(
                originals,
                LocalStorageMode::Managed,
            )),
        )
        .await
        .unwrap();
        (library, recorder)
    }

    /// Open a library whose recorder is built from the library's own
    /// database handle (e.g. a real or failing outbox writer). Returns
    /// the handle too, for direct assertions on `sync_outbox`.
    pub(super) async fn open_test_library_with(
        bundle: Bundle,
        make_recorder: impl FnOnce(Database) -> Arc<dyn MutationRecorder>,
    ) -> (Library, Database) {
        let db = Database::new();
        let recorder = make_recorder(db.clone());
        let originals = bundle.originals.clone();
        let library = Library::open(
            bundle,
            LocalStorageMode::Managed,
            db.clone(),
            recorder,
            Arc::new(resolver::LocalResolver::new(
                originals,
                LocalStorageMode::Managed,
            )),
        )
        .await
        .unwrap();
        (library, db)
    }

    pub(super) fn fresh_bundle(dir: &std::path::Path) -> Bundle {
        let path = dir.join("Test.library");
        Bundle::create(
            &path,
            &LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn open_creates_library() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(
            &bundle_path,
            &LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            },
        )
        .unwrap();

        open_test_library(bundle).await;
    }
}

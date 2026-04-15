pub mod album;
pub mod bundle;
pub mod commands;
pub mod config;
pub mod db;
pub mod editing;
pub mod error;
pub mod faces;
pub mod format;
pub mod media;
pub mod metadata;
pub mod mutation;
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
use media::{MediaId, MediaService};
use metadata::MetadataService;
use recorder::MutationRecorder;
use thumbnail::ThumbnailService;

/// The Moments library — a single concrete type composing all feature services.
///
/// Constructed via [`Library::open`] with a validated [`Bundle`] and
/// [`LocalStorageMode`]. All operations are accessed via service accessors
/// (`media()`, `albums()`, `faces()`, etc.) or through the client layer
/// (`MediaClient`, `AlbumClient`, `PeopleClient`).
pub struct Library {
    albums: AlbumService,
    faces: FacesService,
    editing: EditingService,
    media: MediaService,
    metadata: MetadataService,
    thumbnails: ThumbnailService,
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
        let faces = FacesService::new(db.clone(), None, Arc::clone(&recorder));
        let editing = EditingService::new(db.clone());
        let media = MediaService::new(
            db.clone(),
            bundle.originals.clone(),
            mode,
            recorder,
            resolver,
        );
        let metadata = MetadataService::new(db.clone());
        let thumbnails = ThumbnailService::new(db, bundle.thumbnails.clone());

        debug!("library ready");
        Ok(Self {
            albums,
            faces,
            editing,
            media,
            metadata,
            thumbnails,
        })
    }

    /// Gracefully shut down the library.
    pub async fn close(&self) -> Result<(), LibraryError> {
        info!("closing library");
        Ok(())
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

    // ── Cross-service operations ─────────────────────────────────────

    /// Permanently delete assets: removes files from disk, then DB rows.
    ///
    /// Coordinates across MediaService (file removal + DB delete) and
    /// ThumbnailService (cached thumbnail removal).
    pub async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        for id in ids {
            self.media.remove_original(id).await;
            let thumb = self.thumbnails.thumbnail_path(id);
            if let Err(e) = tokio::fs::remove_file(&thumb).await {
                tracing::debug!(id = %id, "thumbnail not on disk or already removed: {e}");
            }
        }
        self.media.delete_permanently(ids).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::config::LibraryConfig;

    async fn open_test_library(bundle: Bundle) -> Library {
        Library::open(
            bundle,
            LocalStorageMode::Managed,
            Database::new(),
            Arc::new(crate::sync::outbox::NoOpRecorder),
            Arc::new(resolver::LocalResolver::new(
                std::path::PathBuf::new(),
                LocalStorageMode::Managed,
            )),
        )
        .await
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

        let library = open_test_library(bundle).await;
        library.close().await.unwrap();
    }
}

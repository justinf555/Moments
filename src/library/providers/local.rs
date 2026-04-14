use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::runtime::Handle;
use tracing::{debug, info, instrument};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::album::{Album, AlbumId, AlbumService, LibraryAlbums};
use crate::library::bundle::Bundle;
use crate::library::config::LocalStorageMode;
use crate::library::db::Database;
use crate::library::editing::{EditState, EditingService, LibraryEditing};
use crate::library::error::LibraryError;
use crate::library::faces::{FacesService, LibraryFaces, Person, PersonId};
use crate::library::format::{FormatRegistry, RawHandler, StandardHandler, VideoHandler};
use crate::library::import::LibraryImport;
use crate::library::importer::ImportJob;
use crate::library::media::{
    LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaRecord,
};
use crate::library::metadata::{LibraryMetadata, MediaMetadataRecord, MetadataService};
use crate::library::storage::LibraryStorage;
use crate::library::thumbnail::{LibraryThumbnail, ThumbnailService};
use crate::library::viewer::LibraryViewer;

/// Local filesystem backend.
///
/// Holds a Tokio [`Handle`] (the shared application-level library executor)
/// and a [`Database`] connection pool. All I/O-bound work is dispatched
/// through the Tokio handle so it never blocks the GTK main thread.
pub struct LocalLibrary {
    bundle: Bundle,
    mode: LocalStorageMode,
    events: EventSender,
    db: Database,
    albums: AlbumService,
    faces: FacesService,
    editing: EditingService,
    metadata: MetadataService,
    thumbnails: ThumbnailService,
    tokio: Handle,
    formats: Arc<FormatRegistry>,
}

impl LocalLibrary {
    /// Construct and open a local library backend.
    ///
    /// Called by [`crate::library::factory::LibraryFactory`] — not via the
    /// trait method (same pattern as `ImmichLibrary`).
    #[instrument(skip(events, tokio), fields(path = %bundle.path.display(), mode = ?mode))]
    pub async fn open(
        bundle: Bundle,
        mode: LocalStorageMode,
        events: EventSender,
        tokio: Handle,
    ) -> Result<Self, LibraryError> {
        info!("opening local library");

        // Initialise the database on the Tokio executor. DB init is fast
        // (~1ms — schema migration only) so we block briefly here at startup.
        let db_path = bundle.database.join("moments.db");
        let db = tokio
            .spawn(async move { Database::open(&db_path).await })
            .await
            .map_err(|e| LibraryError::Runtime(e.to_string()))??;

        let mut registry = FormatRegistry::new();
        registry.register(Arc::new(StandardHandler));
        registry.register(Arc::new(RawHandler));
        registry.register(Arc::new(VideoHandler));
        let formats = Arc::new(registry);

        let albums = AlbumService::new(db.clone());
        let faces = FacesService::new(db.clone(), None); // local backend: no face thumbnails
        let editing = EditingService::new(db.clone());
        let metadata = MetadataService::new(db.clone());
        let thumbnails = ThumbnailService::new(db.clone(), bundle.thumbnails.clone());

        let library = Self {
            bundle,
            mode,
            events,
            db,
            albums,
            faces,
            editing,
            metadata,
            thumbnails,
            tokio,
            formats,
        };
        // Fire-and-forget: purge items past the configured trash retention period.
        {
            let retention_days = {
                use gtk::prelude::SettingsExt;
                gtk::gio::SettingsSchemaSource::default()
                    .and_then(|src| src.lookup(crate::config::APP_ID, true))
                    .map(|_| {
                        gtk::gio::Settings::new(crate::config::APP_ID).uint("trash-retention-days")
                            as i64
                    })
                    .unwrap_or(30)
            };
            let db = library.db.clone();
            let originals = library.bundle.originals.clone();
            let thumbnails = library.bundle.thumbnails.clone();
            let purge_mode = library.mode.clone();
            library.tokio.spawn(async move {
                let max_age_secs = retention_days * 24 * 60 * 60;
                match db.expired_trash(max_age_secs).await {
                    Ok(ids) if !ids.is_empty() => {
                        info!(count = ids.len(), "auto-purging expired trash");
                        for id in &ids {
                            // Remove original file (managed mode only).
                            match purge_mode {
                                LocalStorageMode::Managed => {
                                    if let Ok(Some(rel)) = db.media_relative_path(id).await {
                                        let path = originals.join(&rel);
                                        let _ = tokio::fs::remove_file(&path).await;
                                    }
                                }
                                LocalStorageMode::Referenced => {
                                    // Referenced mode: the original belongs to the
                                    // user — never delete it.
                                }
                            }
                            // Remove thumbnail file (always owned by Moments).
                            let thumb =
                                crate::library::thumbnail::sharded_thumbnail_path(&thumbnails, id);
                            let _ = tokio::fs::remove_file(&thumb).await;
                        }
                        if let Err(e) = db.delete_permanently(&ids).await {
                            tracing::error!("auto-purge DB cleanup failed: {e}");
                        }
                    }
                    Ok(_) => debug!("no expired trash to purge"),
                    Err(e) => tracing::error!("auto-purge query failed: {e}"),
                }
            });
        }

        library.events.send(AppEvent::Ready);
        debug!("local library ready");
        Ok(library)
    }
}

#[async_trait]
impl LibraryStorage for LocalLibrary {
    async fn open(
        _bundle: Bundle,
        _events: EventSender,
        _tokio: Handle,
    ) -> Result<Self, LibraryError>
    where
        Self: Sized,
    {
        // Not reachable — factory calls LocalLibrary::open directly.
        Err(LibraryError::Bundle(
            "use LocalLibrary::open() instead of LibraryStorage::open()".to_string(),
        ))
    }

    #[instrument(skip(self), fields(path = %self.bundle.path.display()))]
    async fn close(&self) -> Result<(), LibraryError> {
        info!("closing local library");
        self.events.send(AppEvent::ShutdownComplete);
        Ok(())
    }
}

#[async_trait]
impl LibraryImport for LocalLibrary {
    #[instrument(skip(self), fields(source_count = sources.len()))]
    async fn import(&self, sources: Vec<PathBuf>) -> Result<(), LibraryError> {
        info!("starting import");
        let job = ImportJob::new(
            self.bundle.originals.clone(),
            self.bundle.thumbnails.clone(),
            self.db.clone(),
            self.events.clone(),
            Arc::clone(&self.formats),
            self.mode.clone(),
        );
        self.tokio.spawn(async move { job.run(sources).await });
        Ok(())
    }
}

#[async_trait]
impl LibraryMedia for LocalLibrary {
    async fn get_media_item(&self, id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        self.db.get_media_item(id).await
    }

    async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.db.media_exists(id).await
    }

    async fn insert_media(&self, record: &MediaRecord) -> Result<(), LibraryError> {
        self.db.insert_media(record).await
    }

    async fn list_media(
        &self,
        filter: MediaFilter,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        self.db.list_media(filter, cursor, limit).await
    }

    async fn set_favorite(&self, ids: &[MediaId], favorite: bool) -> Result<(), LibraryError> {
        self.db.set_favorite(ids, favorite).await
    }

    async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.db.trash(ids).await
    }

    async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.db.restore(ids).await
    }

    async fn delete_permanently(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        // Remove files from disk before deleting DB rows.
        for id in ids {
            match self.mode {
                LocalStorageMode::Managed => {
                    if let Ok(Some(rel)) = self.db.media_relative_path(id).await {
                        let full = self.bundle.originals.join(&rel);
                        if let Err(e) = tokio::fs::remove_file(&full).await {
                            tracing::warn!(id = %id, path = %full.display(), "failed to remove original: {e}");
                        }
                    }
                }
                LocalStorageMode::Referenced => {
                    // Referenced mode: the original belongs to the user — don't delete it.
                    debug!(id = %id, "referenced mode: skipping original file deletion");
                }
            }
            // Remove thumbnail file (always owned by Moments).
            let thumb = self.thumbnail_path(id);
            if let Err(e) = tokio::fs::remove_file(&thumb).await {
                tracing::debug!(id = %id, "thumbnail not on disk or already removed: {e}");
            }
        }
        self.db.delete_permanently(ids).await
    }

    async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        self.db.expired_trash(max_age_secs).await
    }

    async fn library_stats(&self) -> Result<crate::library::db::LibraryStats, LibraryError> {
        self.db.library_stats().await
    }
}

#[async_trait]
impl LibraryMetadata for LocalLibrary {
    async fn insert_media_metadata(
        &self,
        record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        self.metadata.insert_media_metadata(record).await
    }

    async fn media_metadata(
        &self,
        id: &MediaId,
    ) -> Result<Option<MediaMetadataRecord>, LibraryError> {
        self.metadata.media_metadata(id).await
    }
}

#[async_trait]
impl LibraryViewer for LocalLibrary {
    async fn original_path(
        &self,
        id: &MediaId,
    ) -> Result<Option<std::path::PathBuf>, LibraryError> {
        let stored = self.db.media_relative_path(id).await?;
        Ok(stored.map(|p| match self.mode {
            // Referenced mode: the DB stores the absolute (portal) path.
            LocalStorageMode::Referenced => PathBuf::from(p),
            // Managed mode: the DB stores a relative path under originals/.
            LocalStorageMode::Managed => self.bundle.originals.join(p),
        }))
    }
}

#[async_trait]
impl LibraryThumbnail for LocalLibrary {
    fn thumbnail_path(&self, id: &MediaId) -> std::path::PathBuf {
        self.thumbnails.thumbnail_path(id)
    }

    async fn insert_thumbnail_pending(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.thumbnails.insert_thumbnail_pending(id).await
    }

    async fn set_thumbnail_ready(
        &self,
        id: &MediaId,
        file_path: &str,
        generated_at: i64,
    ) -> Result<(), LibraryError> {
        self.thumbnails
            .set_thumbnail_ready(id, file_path, generated_at)
            .await
    }

    async fn set_thumbnail_failed(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.thumbnails.set_thumbnail_failed(id).await
    }

    async fn thumbnail_status(
        &self,
        id: &MediaId,
    ) -> Result<Option<crate::library::thumbnail::ThumbnailStatus>, LibraryError> {
        self.thumbnails.thumbnail_status(id).await
    }
}

#[async_trait]
impl LibraryAlbums for LocalLibrary {
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        self.albums.list_albums().await
    }

    async fn create_album(&self, name: &str) -> Result<AlbumId, LibraryError> {
        self.albums.create_album(name).await
    }

    async fn rename_album(&self, id: &AlbumId, name: &str) -> Result<(), LibraryError> {
        self.albums.rename_album(id, name).await
    }

    async fn delete_album(&self, id: &AlbumId) -> Result<(), LibraryError> {
        self.albums.delete_album(id).await
    }

    async fn add_to_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.albums.add_to_album(album_id, media_ids).await
    }

    async fn remove_from_album(
        &self,
        album_id: &AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.albums.remove_from_album(album_id, media_ids).await
    }

    async fn list_album_media(
        &self,
        album_id: &AlbumId,
        cursor: Option<&MediaCursor>,
        limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        self.albums.list_album_media(album_id, cursor, limit).await
    }

    async fn albums_containing_media(
        &self,
        media_ids: &[MediaId],
    ) -> Result<std::collections::HashMap<AlbumId, usize>, LibraryError> {
        self.albums.albums_containing_media(media_ids).await
    }

    async fn album_cover_media_ids(
        &self,
        album_id: &AlbumId,
        limit: u32,
    ) -> Result<Vec<MediaId>, LibraryError> {
        self.albums.album_cover_media_ids(album_id, limit).await
    }
}

#[async_trait]
impl LibraryFaces for LocalLibrary {
    async fn list_people(
        &self,
        include_hidden: bool,
        include_unnamed: bool,
    ) -> Result<Vec<Person>, LibraryError> {
        self.faces
            .list_people(include_hidden, include_unnamed)
            .await
    }

    async fn list_media_for_person(
        &self,
        person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError> {
        self.faces.list_media_for_person(person_id).await
    }

    async fn rename_person(&self, person_id: &PersonId, name: &str) -> Result<(), LibraryError> {
        self.faces.rename_person(person_id, name).await
    }

    async fn set_person_hidden(
        &self,
        person_id: &PersonId,
        hidden: bool,
    ) -> Result<(), LibraryError> {
        self.faces.set_person_hidden(person_id, hidden).await
    }

    async fn merge_people(
        &self,
        target: &PersonId,
        sources: &[PersonId],
    ) -> Result<(), LibraryError> {
        self.faces.merge_people(target, sources).await
    }

    fn person_thumbnail_path(&self, person_id: &PersonId) -> Option<std::path::PathBuf> {
        self.faces.person_thumbnail_path(person_id)
    }
}

#[async_trait]
impl LibraryEditing for LocalLibrary {
    async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError> {
        self.editing.get_edit_state(id).await
    }

    async fn save_edit_state(&self, id: &MediaId, state: &EditState) -> Result<(), LibraryError> {
        self.editing.save_edit_state(id, state).await
    }

    async fn revert_edits(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.editing.revert_edits(id).await
    }

    async fn render_and_save(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.editing.render_and_save(id).await
    }

    async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.editing.has_pending_edits(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crate::event_bus::EventSender;
    use crate::library::config::{LibraryConfig, LocalStorageMode};
    use tempfile::tempdir;

    async fn open_test_library(bundle: Bundle, tx: EventSender) -> LocalLibrary {
        let handle = tokio::runtime::Handle::current();
        LocalLibrary::open(bundle, LocalStorageMode::Managed, tx, handle)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_sends_ready_event() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(
            &bundle_path,
            &LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            },
        )
        .unwrap();

        let (tx, rx) = EventSender::test_channel();
        let _library = open_test_library(bundle, tx).await;

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Ready));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_sends_shutdown_complete() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(
            &bundle_path,
            &LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            },
        )
        .unwrap();

        let (tx, rx) = EventSender::test_channel();
        let library = open_test_library(bundle, tx).await;
        rx.try_recv().unwrap(); // consume Ready

        library.close().await.unwrap();
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::ShutdownComplete));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn import_emits_complete_event() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(
            &bundle_path,
            &LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            },
        )
        .unwrap();

        let src_dir = tempdir().unwrap();
        std::fs::write(src_dir.path().join("img.jpg"), b"fake").unwrap();

        let (tx, rx) = EventSender::test_channel();
        let library = open_test_library(bundle, tx).await;
        rx.try_recv().unwrap(); // consume Ready

        library
            .import(vec![src_dir.path().to_path_buf()])
            .await
            .unwrap();

        // import() spawns on Tokio; drain events until ImportComplete arrives.
        let has_complete = loop {
            match rx.recv().unwrap() {
                AppEvent::ImportComplete { .. } => break true,
                _ => continue,
            }
        };
        assert!(has_complete);
    }
}

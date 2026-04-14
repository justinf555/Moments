pub mod album;
pub mod bundle;
pub mod commands;
pub mod config;
pub mod db;
pub mod editing;
pub mod error;
pub mod faces;
pub mod format;
pub mod keyring;
pub mod media;
pub mod metadata;
pub mod thumbnail;

use std::path::PathBuf;
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
use thumbnail::ThumbnailService;

/// The Moments library — a single concrete type composing all feature services.
///
/// Constructed via [`Library::open`] with a validated [`Bundle`] and
/// [`LocalStorageMode`]. All operations delegate to the underlying services;
/// the Library itself holds no database handle or runtime state beyond them.
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
    #[instrument(skip_all, fields(path = %bundle.path.display(), mode = ?mode))]
    pub async fn open(bundle: Bundle, mode: LocalStorageMode) -> Result<Self, LibraryError> {
        info!("opening library");

        let db_path = bundle.database.join("moments.db");
        let db = Database::open(&db_path).await?;

        let albums = AlbumService::new(db.clone());
        let faces = FacesService::new(db.clone(), None);
        let editing = EditingService::new(db.clone());
        let media = MediaService::new(db.clone(), bundle.originals.clone(), mode);
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

    // ── Media ───────────────────────────────────────────────────────

    pub async fn get_media_item(
        &self,
        id: &MediaId,
    ) -> Result<Option<media::MediaItem>, LibraryError> {
        self.media.get_media_item(id).await
    }

    pub async fn media_exists(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.media.media_exists(id).await
    }

    pub async fn insert_media(&self, record: &media::MediaRecord) -> Result<(), LibraryError> {
        self.media.insert_media(record).await
    }

    pub async fn list_media(
        &self,
        filter: media::MediaFilter,
        cursor: Option<&media::MediaCursor>,
        limit: u32,
    ) -> Result<Vec<media::MediaItem>, LibraryError> {
        self.media.list_media(filter, cursor, limit).await
    }

    pub async fn set_favorite(&self, ids: &[MediaId], favorite: bool) -> Result<(), LibraryError> {
        self.media.set_favorite(ids, favorite).await
    }

    pub async fn trash(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.media.trash(ids).await
    }

    pub async fn restore(&self, ids: &[MediaId]) -> Result<(), LibraryError> {
        self.media.restore(ids).await
    }

    /// Permanently delete assets: removes files from disk, then DB rows.
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

    pub async fn expired_trash(&self, max_age_secs: i64) -> Result<Vec<MediaId>, LibraryError> {
        self.media.expired_trash(max_age_secs).await
    }

    pub async fn library_stats(&self) -> Result<db::LibraryStats, LibraryError> {
        self.media.library_stats().await
    }

    // ── Viewer ──────────────────────────────────────────────────────

    pub async fn original_path(&self, id: &MediaId) -> Result<Option<PathBuf>, LibraryError> {
        self.media.original_path(id).await
    }

    // ── Metadata ────────────────────────────────────────────────────

    pub async fn insert_media_metadata(
        &self,
        record: &metadata::MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        self.metadata.insert_media_metadata(record).await
    }

    pub async fn media_metadata(
        &self,
        id: &MediaId,
    ) -> Result<Option<metadata::MediaMetadataRecord>, LibraryError> {
        self.metadata.media_metadata(id).await
    }

    // ── Thumbnails ──────────────────────────────────────────────────

    pub fn thumbnail_path(&self, id: &MediaId) -> PathBuf {
        self.thumbnails.thumbnail_path(id)
    }

    // ── Albums ──────────────────────────────────────────────────────

    pub async fn list_albums(&self) -> Result<Vec<album::Album>, LibraryError> {
        self.albums.list_albums().await
    }

    pub async fn create_album(&self, name: &str) -> Result<album::AlbumId, LibraryError> {
        self.albums.create_album(name).await
    }

    pub async fn rename_album(&self, id: &album::AlbumId, name: &str) -> Result<(), LibraryError> {
        self.albums.rename_album(id, name).await
    }

    pub async fn delete_album(&self, id: &album::AlbumId) -> Result<(), LibraryError> {
        self.albums.delete_album(id).await
    }

    pub async fn add_to_album(
        &self,
        album_id: &album::AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.albums.add_to_album(album_id, media_ids).await
    }

    pub async fn remove_from_album(
        &self,
        album_id: &album::AlbumId,
        media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.albums.remove_from_album(album_id, media_ids).await
    }

    pub async fn list_album_media(
        &self,
        album_id: &album::AlbumId,
        cursor: Option<&media::MediaCursor>,
        limit: u32,
    ) -> Result<Vec<media::MediaItem>, LibraryError> {
        self.albums.list_album_media(album_id, cursor, limit).await
    }

    pub async fn albums_containing_media(
        &self,
        media_ids: &[MediaId],
    ) -> Result<std::collections::HashMap<album::AlbumId, usize>, LibraryError> {
        self.albums.albums_containing_media(media_ids).await
    }

    pub async fn album_cover_media_ids(
        &self,
        album_id: &album::AlbumId,
        limit: u32,
    ) -> Result<Vec<MediaId>, LibraryError> {
        self.albums.album_cover_media_ids(album_id, limit).await
    }

    // ── Faces ───────────────────────────────────────────────────────

    pub async fn list_people(
        &self,
        include_hidden: bool,
        include_unnamed: bool,
    ) -> Result<Vec<faces::Person>, LibraryError> {
        self.faces
            .list_people(include_hidden, include_unnamed)
            .await
    }

    pub async fn list_media_for_person(
        &self,
        person_id: &faces::PersonId,
    ) -> Result<Vec<MediaId>, LibraryError> {
        self.faces.list_media_for_person(person_id).await
    }

    pub async fn rename_person(
        &self,
        person_id: &faces::PersonId,
        name: &str,
    ) -> Result<(), LibraryError> {
        self.faces.rename_person(person_id, name).await
    }

    pub async fn set_person_hidden(
        &self,
        person_id: &faces::PersonId,
        hidden: bool,
    ) -> Result<(), LibraryError> {
        self.faces.set_person_hidden(person_id, hidden).await
    }

    pub async fn merge_people(
        &self,
        target: &faces::PersonId,
        sources: &[faces::PersonId],
    ) -> Result<(), LibraryError> {
        self.faces.merge_people(target, sources).await
    }

    pub fn person_thumbnail_path(&self, person_id: &faces::PersonId) -> Option<PathBuf> {
        self.faces.person_thumbnail_path(person_id)
    }

    // ── Editing ─────────────────────────────────────────────────────

    pub async fn get_edit_state(
        &self,
        id: &MediaId,
    ) -> Result<Option<editing::EditState>, LibraryError> {
        self.editing.get_edit_state(id).await
    }

    pub async fn save_edit_state(
        &self,
        id: &MediaId,
        state: &editing::EditState,
    ) -> Result<(), LibraryError> {
        self.editing.save_edit_state(id, state).await
    }

    pub async fn revert_edits(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.editing.revert_edits(id).await
    }

    pub async fn render_and_save(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.editing.render_and_save(id).await
    }

    pub async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError> {
        self.editing.has_pending_edits(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::config::LibraryConfig;

    async fn open_test_library(bundle: Bundle) -> Library {
        Library::open(bundle, LocalStorageMode::Managed)
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

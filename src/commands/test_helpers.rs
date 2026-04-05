//! Test helpers for command handler unit tests.
//!
//! Provides a configurable mock library that returns `Ok(())` or a fixed
//! error for each operation, and a capturing event sender.

#![cfg(test)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::library::album::{Album, AlbumId, LibraryAlbums};
use crate::library::bundle::Bundle;
use crate::library::db::LibraryStats;
use crate::library::editing::{EditState, LibraryEditing};
use crate::library::error::LibraryError;
use crate::library::event::LibraryEvent;
use crate::library::faces::{LibraryFaces, Person, PersonId};
use crate::library::import::LibraryImport;
use crate::library::media::{
    LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaMetadataRecord, MediaRecord,
};
use crate::library::storage::LibraryStorage;
use crate::library::thumbnail::{LibraryThumbnail, ThumbnailStatus};
use crate::library::viewer::LibraryViewer;

/// Mock library that records calls and returns configurable results.
pub struct MockLibrary {
    /// If `Some`, all mutable operations return this error.
    pub fail_with: Mutex<Option<String>>,
    /// Album ID returned by `create_album`.
    pub next_album_id: Mutex<AlbumId>,
}

impl MockLibrary {
    pub fn mock() -> Arc<dyn crate::library::Library> {
        Arc::new(Self {
            fail_with: Mutex::new(None),
            next_album_id: Mutex::new(AlbumId::new()),
        })
    }

    pub fn mock_failing(msg: &str) -> Arc<dyn crate::library::Library> {
        Arc::new(Self {
            fail_with: Mutex::new(Some(msg.to_string())),
            next_album_id: Mutex::new(AlbumId::new()),
        })
    }

    async fn check_fail(&self) -> Result<(), LibraryError> {
        if let Some(msg) = self.fail_with.lock().await.as_ref() {
            Err(LibraryError::Runtime(msg.clone()))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl LibraryStorage for MockLibrary {
    async fn open(
        _bundle: Bundle,
        _events: std::sync::mpsc::Sender<LibraryEvent>,
        _tokio: tokio::runtime::Handle,
    ) -> Result<Self, LibraryError>
    where
        Self: Sized,
    {
        unimplemented!()
    }
    async fn close(&self) -> Result<(), LibraryError> {
        Ok(())
    }
}

#[async_trait]
impl LibraryImport for MockLibrary {
    async fn import(&self, _sources: Vec<PathBuf>) -> Result<(), LibraryError> {
        unimplemented!()
    }
}

#[async_trait]
impl LibraryMedia for MockLibrary {
    async fn media_exists(&self, _id: &MediaId) -> Result<bool, LibraryError> {
        unimplemented!()
    }
    async fn get_media_item(&self, _id: &MediaId) -> Result<Option<MediaItem>, LibraryError> {
        unimplemented!()
    }
    async fn insert_media(&self, _record: &MediaRecord) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn insert_media_metadata(
        &self,
        _record: &MediaMetadataRecord,
    ) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn list_media(
        &self,
        _filter: MediaFilter,
        _cursor: Option<&MediaCursor>,
        _limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        unimplemented!()
    }
    async fn media_metadata(
        &self,
        _id: &MediaId,
    ) -> Result<Option<MediaMetadataRecord>, LibraryError> {
        unimplemented!()
    }
    async fn set_favorite(&self, _ids: &[MediaId], _fav: bool) -> Result<(), LibraryError> {
        self.check_fail().await
    }
    async fn trash(&self, _ids: &[MediaId]) -> Result<(), LibraryError> {
        self.check_fail().await
    }
    async fn restore(&self, _ids: &[MediaId]) -> Result<(), LibraryError> {
        self.check_fail().await
    }
    async fn delete_permanently(&self, _ids: &[MediaId]) -> Result<(), LibraryError> {
        self.check_fail().await
    }
    async fn expired_trash(&self, _max_age: i64) -> Result<Vec<MediaId>, LibraryError> {
        unimplemented!()
    }
    async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        unimplemented!()
    }
}

#[async_trait]
impl LibraryThumbnail for MockLibrary {
    fn thumbnail_path(&self, _id: &MediaId) -> PathBuf {
        unimplemented!()
    }
    async fn insert_thumbnail_pending(&self, _id: &MediaId) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn set_thumbnail_ready(
        &self,
        _id: &MediaId,
        _path: &str,
        _at: i64,
    ) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn set_thumbnail_failed(&self, _id: &MediaId) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn thumbnail_status(
        &self,
        _id: &MediaId,
    ) -> Result<Option<ThumbnailStatus>, LibraryError> {
        unimplemented!()
    }
}

#[async_trait]
impl LibraryViewer for MockLibrary {
    async fn original_path(&self, _id: &MediaId) -> Result<Option<PathBuf>, LibraryError> {
        unimplemented!()
    }
}

#[async_trait]
impl LibraryAlbums for MockLibrary {
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        unimplemented!()
    }
    async fn create_album(&self, _name: &str) -> Result<AlbumId, LibraryError> {
        self.check_fail().await?;
        Ok(self.next_album_id.lock().await.clone())
    }
    async fn rename_album(&self, _id: &AlbumId, _name: &str) -> Result<(), LibraryError> {
        self.check_fail().await
    }
    async fn delete_album(&self, _id: &AlbumId) -> Result<(), LibraryError> {
        self.check_fail().await
    }
    async fn add_to_album(
        &self,
        _album_id: &AlbumId,
        _media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.check_fail().await
    }
    async fn remove_from_album(
        &self,
        _album_id: &AlbumId,
        _media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        self.check_fail().await
    }
    async fn list_album_media(
        &self,
        _album_id: &AlbumId,
        _cursor: Option<&MediaCursor>,
        _limit: u32,
    ) -> Result<Vec<MediaItem>, LibraryError> {
        unimplemented!()
    }
    async fn albums_containing_media(
        &self,
        _media_ids: &[MediaId],
    ) -> Result<std::collections::HashMap<AlbumId, usize>, LibraryError> {
        Ok(std::collections::HashMap::new())
    }
    async fn album_cover_media_ids(
        &self,
        _album_id: &AlbumId,
        _limit: u32,
    ) -> Result<Vec<MediaId>, LibraryError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl LibraryFaces for MockLibrary {
    async fn list_people(
        &self,
        _include_hidden: bool,
        _include_unnamed: bool,
    ) -> Result<Vec<Person>, LibraryError> {
        unimplemented!()
    }
    async fn list_media_for_person(
        &self,
        _person_id: &PersonId,
    ) -> Result<Vec<MediaId>, LibraryError> {
        unimplemented!()
    }
    async fn rename_person(
        &self,
        _person_id: &PersonId,
        _name: &str,
    ) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn set_person_hidden(
        &self,
        _person_id: &PersonId,
        _hidden: bool,
    ) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn merge_people(
        &self,
        _target: &PersonId,
        _sources: &[PersonId],
    ) -> Result<(), LibraryError> {
        unimplemented!()
    }
    fn person_thumbnail_path(&self, _person_id: &PersonId) -> Option<PathBuf> {
        unimplemented!()
    }
}

#[async_trait]
impl LibraryEditing for MockLibrary {
    async fn get_edit_state(
        &self,
        _id: &MediaId,
    ) -> Result<Option<EditState>, LibraryError> {
        unimplemented!()
    }
    async fn save_edit_state(
        &self,
        _id: &MediaId,
        _state: &EditState,
    ) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn revert_edits(&self, _id: &MediaId) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn render_and_save(&self, _id: &MediaId) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn has_pending_edits(&self, _id: &MediaId) -> Result<bool, LibraryError> {
        unimplemented!()
    }
}

//! A stub Library implementation for integration tests.
//!
//! All methods return `unimplemented!()` by default. Tests that exercise
//! synchronous model operations (insert, remove, property updates) never
//! call these methods — they exist only to satisfy the trait bounds on
//! `PhotoGridModel::new()`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use moments::library::album::{Album, AlbumId, LibraryAlbums};
use moments::library::bundle::Bundle;
use moments::library::db::LibraryStats;
use moments::library::editing::{EditState, LibraryEditing};
use moments::library::error::LibraryError;
use moments::library::event::LibraryEvent;
use moments::library::faces::{LibraryFaces, Person, PersonId};
use moments::library::import::LibraryImport;
use moments::library::media::{
    LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaMetadataRecord, MediaRecord,
};
use moments::library::storage::LibraryStorage;
use moments::library::thumbnail::{LibraryThumbnail, ThumbnailStatus};
use moments::library::viewer::LibraryViewer;
use moments::library::Library;

pub struct StubLibrary;

/// Create a stub Library and a Tokio Handle for use in tests.
pub fn stub_deps() -> (Arc<dyn Library>, tokio::runtime::Handle) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = rt.handle().clone();
    // Leak the runtime so it stays alive for the test.
    std::mem::forget(rt);
    (Arc::new(StubLibrary) as Arc<dyn Library>, handle)
}

#[async_trait]
impl LibraryStorage for StubLibrary {
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
        unimplemented!()
    }
}

#[async_trait]
impl LibraryImport for StubLibrary {
    async fn import(&self, _sources: Vec<PathBuf>) -> Result<(), LibraryError> {
        unimplemented!()
    }
}

#[async_trait]
impl LibraryMedia for StubLibrary {
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
        unimplemented!()
    }
    async fn trash(&self, _ids: &[MediaId]) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn restore(&self, _ids: &[MediaId]) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn delete_permanently(&self, _ids: &[MediaId]) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn expired_trash(&self, _max_age: i64) -> Result<Vec<MediaId>, LibraryError> {
        unimplemented!()
    }
    async fn library_stats(&self) -> Result<LibraryStats, LibraryError> {
        unimplemented!()
    }
}

#[async_trait]
impl LibraryThumbnail for StubLibrary {
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
impl LibraryViewer for StubLibrary {
    async fn original_path(&self, _id: &MediaId) -> Result<Option<PathBuf>, LibraryError> {
        unimplemented!()
    }
}

#[async_trait]
impl LibraryAlbums for StubLibrary {
    async fn list_albums(&self) -> Result<Vec<Album>, LibraryError> {
        unimplemented!()
    }
    async fn create_album(&self, _name: &str) -> Result<AlbumId, LibraryError> {
        unimplemented!()
    }
    async fn rename_album(&self, _id: &AlbumId, _name: &str) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn delete_album(&self, _id: &AlbumId) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn add_to_album(
        &self,
        _album_id: &AlbumId,
        _media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        unimplemented!()
    }
    async fn remove_from_album(
        &self,
        _album_id: &AlbumId,
        _media_ids: &[MediaId],
    ) -> Result<(), LibraryError> {
        unimplemented!()
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
}

#[async_trait]
impl LibraryFaces for StubLibrary {
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
impl LibraryEditing for StubLibrary {
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

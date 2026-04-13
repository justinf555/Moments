use std::sync::Arc;

use tracing::{debug, error, info};

use crate::app_event::AppEvent;
use crate::event_bus::{EventBus, EventSender, Subscription};
use crate::library::media::MediaFilter;
use crate::library::Library;

/// Subscribe to command events and handle them on the Tokio runtime.
///
/// Replaces the old `CommandDispatcher` — the library subscribes directly
/// to `*Requested` events and emits result events via the bus.
pub fn subscribe_commands(
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    bus: &EventBus,
) -> Subscription {
    let tx = bus.sender();

    bus.subscribe(move |event| {
        if !is_command(event) {
            return;
        }
        let lib = Arc::clone(&library);
        let bus_tx = tx.clone();
        let evt = event.clone();
        tokio.spawn(async move {
            handle_command(evt, &lib, &bus_tx).await;
        });
    })
}

/// Returns true if the event is a command that should be handled.
fn is_command(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::TrashRequested { .. }
            | AppEvent::RestoreRequested { .. }
            | AppEvent::DeleteRequested { .. }
            | AppEvent::FavoriteRequested { .. }
            | AppEvent::RemoveFromAlbumRequested { .. }
            | AppEvent::AddToAlbumRequested { .. }
            | AppEvent::CreateAlbumRequested { .. }
            | AppEvent::DeleteAlbumRequested { .. }
            | AppEvent::EmptyTrashRequested
            | AppEvent::RestoreAllTrashRequested
    )
}

/// Execute a command event on the Tokio runtime.
///
/// On success, sends the result event via the bus sender.
/// On failure, sends `AppEvent::Error` with a user-facing message.
pub(crate) async fn handle_command(event: AppEvent, library: &Arc<dyn Library>, bus: &EventSender) {
    match event {
        AppEvent::TrashRequested { ids } => match library.trash(&ids).await {
            Ok(()) => bus.send(AppEvent::Trashed { ids }),
            Err(e) => {
                error!("trash failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to move to trash: {e}")));
            }
        },

        AppEvent::RestoreRequested { ids } => match library.restore(&ids).await {
            Ok(()) => bus.send(AppEvent::Restored { ids }),
            Err(e) => {
                error!("restore failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to restore: {e}")));
            }
        },

        AppEvent::DeleteRequested { ids } => match library.delete_permanently(&ids).await {
            Ok(()) => bus.send(AppEvent::Deleted { ids }),
            Err(e) => {
                error!("delete permanently failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to delete: {e}")));
            }
        },

        AppEvent::FavoriteRequested { ids, state } => match library.set_favorite(&ids, state).await
        {
            Ok(()) => bus.send(AppEvent::FavoriteChanged {
                ids,
                is_favorite: state,
            }),
            Err(e) => {
                error!("set_favorite failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to update favourite: {e}")));
            }
        },

        AppEvent::RemoveFromAlbumRequested { album_id, ids } => {
            match library.remove_from_album(&album_id, &ids).await {
                Ok(()) => bus.send(AppEvent::AlbumMediaChanged { album_id }),
                Err(e) => {
                    error!("remove_from_album failed: {e}");
                    bus.send(AppEvent::Error(format!("Failed to remove from album: {e}")));
                }
            }
        }

        AppEvent::AddToAlbumRequested { album_id, ids } => {
            match library.add_to_album(&album_id, &ids).await {
                Ok(()) => bus.send(AppEvent::AlbumMediaChanged { album_id }),
                Err(e) => {
                    error!("add_to_album failed: {e}");
                    bus.send(AppEvent::Error(format!("Failed to add to album: {e}")));
                }
            }
        }

        AppEvent::CreateAlbumRequested { name, ids } => match library.create_album(&name).await {
            Ok(album_id) => {
                debug!(album_id = %album_id, %name, "album created");
                bus.send(AppEvent::AlbumCreated {
                    id: album_id.clone(),
                    name,
                });
                if !ids.is_empty() {
                    match library.add_to_album(&album_id, &ids).await {
                        Ok(()) => {
                            debug!(album_id = %album_id, "photos added to new album");
                            bus.send(AppEvent::AlbumMediaChanged { album_id });
                        }
                        Err(e) => {
                            error!("add_to_album after create failed: {e}");
                            bus.send(AppEvent::Error(format!("Failed to add to album: {e}")));
                        }
                    }
                }
            }
            Err(e) => {
                error!("create_album failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to create album: {e}")));
            }
        },

        AppEvent::DeleteAlbumRequested { ids } => {
            for id in ids {
                match library.delete_album(&id).await {
                    Ok(()) => {
                        debug!(album_id = %id, "album deleted");
                        bus.send(AppEvent::AlbumDeleted { id });
                    }
                    Err(e) => {
                        error!(album_id = %id, "delete_album failed: {e}");
                        bus.send(AppEvent::Error(format!("Failed to delete album: {e}")));
                    }
                }
            }
        }

        AppEvent::EmptyTrashRequested => {
            let items = match library
                .list_media(MediaFilter::Trashed, None, u32::MAX)
                .await
            {
                Ok(items) => items,
                Err(e) => {
                    error!("failed to list trashed items: {e}");
                    bus.send(AppEvent::Error(format!("Failed to empty trash: {e}")));
                    return;
                }
            };
            if items.is_empty() {
                return;
            }
            let ids: Vec<_> = items.into_iter().map(|i| i.id).collect();
            let count = ids.len();
            match library.delete_permanently(&ids).await {
                Ok(()) => {
                    info!(count, "trash emptied");
                    bus.send(AppEvent::Deleted { ids });
                }
                Err(e) => {
                    error!("empty trash failed: {e}");
                    bus.send(AppEvent::Error(format!("Failed to empty trash: {e}")));
                }
            }
        }

        AppEvent::RestoreAllTrashRequested => {
            let items = match library
                .list_media(MediaFilter::Trashed, None, u32::MAX)
                .await
            {
                Ok(items) => items,
                Err(e) => {
                    error!("failed to list trashed items: {e}");
                    bus.send(AppEvent::Error(format!("Failed to restore trash: {e}")));
                    return;
                }
            };
            if items.is_empty() {
                return;
            }
            let ids: Vec<_> = items.into_iter().map(|i| i.id).collect();
            let count = ids.len();
            match library.restore(&ids).await {
                Ok(()) => {
                    info!(count, "all trash restored");
                    bus.send(AppEvent::Restored { ids });
                }
                Err(e) => {
                    error!("restore all trash failed: {e}");
                    bus.send(AppEvent::Error(format!("Failed to restore: {e}")));
                }
            }
        }

        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::album::{Album, AlbumId, LibraryAlbums};
    use crate::library::bundle::Bundle;
    use crate::library::db::LibraryStats;
    use crate::library::editing::{EditState, LibraryEditing};
    use crate::library::error::LibraryError;
    use crate::library::faces::{LibraryFaces, Person, PersonId};
    use crate::library::import::LibraryImport;
    use crate::library::media::{
        LibraryMedia, MediaCursor, MediaFilter, MediaId, MediaItem, MediaMetadataRecord,
        MediaRecord, MediaType,
    };
    use crate::library::storage::LibraryStorage;
    use crate::library::thumbnail::{LibraryThumbnail, ThumbnailStatus};
    use crate::library::viewer::LibraryViewer;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use tokio::sync::Mutex;

    /// Mock library that records calls and returns configurable results.
    pub struct MockLibrary {
        pub fail_with: Mutex<Option<String>>,
        pub items: Mutex<Vec<MediaItem>>,
        pub fail_add_to_album: Mutex<bool>,
        pub next_album_id: Mutex<AlbumId>,
    }

    impl MockLibrary {
        pub fn mock() -> Arc<dyn Library> {
            Arc::new(Self {
                fail_with: Mutex::new(None),
                items: Mutex::new(Vec::new()),
                fail_add_to_album: Mutex::new(false),
                next_album_id: Mutex::new(AlbumId::new()),
            })
        }

        pub fn mock_failing(msg: &str) -> Arc<dyn Library> {
            Arc::new(Self {
                fail_with: Mutex::new(Some(msg.to_string())),
                items: Mutex::new(Vec::new()),
                fail_add_to_album: Mutex::new(false),
                next_album_id: Mutex::new(AlbumId::new()),
            })
        }

        pub fn mock_with_items_then_fail(items: Vec<MediaItem>, msg: &str) -> Arc<dyn Library> {
            Arc::new(Self {
                fail_with: Mutex::new(Some(msg.to_string())),
                items: Mutex::new(items),
                fail_add_to_album: Mutex::new(false),
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
            _events: crate::event_bus::EventSender,
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
            Ok(self.items.lock().await.clone())
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
            self.check_fail().await?;
            if *self.fail_add_to_album.lock().await {
                return Err(LibraryError::Runtime("add_to_album failed".into()));
            }
            Ok(())
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
        async fn get_edit_state(&self, _id: &MediaId) -> Result<Option<EditState>, LibraryError> {
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

    // ── Tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn trash_success_emits_trashed() {
        let lib = MockLibrary::mock();
        let (bus, rx) = EventSender::test_channel();
        let ids = vec![MediaId::new("abc".into())];
        handle_command(AppEvent::TrashRequested { ids: ids.clone() }, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Trashed { ids: ref got } if got == &ids));
    }

    #[tokio::test]
    async fn trash_failure_emits_error() {
        let lib = MockLibrary::mock_failing("db error");
        let (bus, rx) = EventSender::test_channel();
        handle_command(
            AppEvent::TrashRequested {
                ids: vec![MediaId::new("x".into())],
            },
            &lib,
            &bus,
        )
        .await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("trash")));
    }

    #[tokio::test]
    async fn restore_success_emits_restored() {
        let lib = MockLibrary::mock();
        let (bus, rx) = EventSender::test_channel();
        let ids = vec![MediaId::new("abc".into())];
        handle_command(AppEvent::RestoreRequested { ids: ids.clone() }, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Restored { ids: ref got } if got == &ids));
    }

    #[tokio::test]
    async fn delete_success_emits_deleted() {
        let lib = MockLibrary::mock();
        let (bus, rx) = EventSender::test_channel();
        let ids = vec![MediaId::new("abc".into())];
        handle_command(AppEvent::DeleteRequested { ids: ids.clone() }, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Deleted { ids: ref got } if got == &ids));
    }

    #[tokio::test]
    async fn favorite_success_emits_changed() {
        let lib = MockLibrary::mock();
        let (bus, rx) = EventSender::test_channel();
        let ids = vec![MediaId::new("abc".into())];
        handle_command(
            AppEvent::FavoriteRequested {
                ids: ids.clone(),
                state: true,
            },
            &lib,
            &bus,
        )
        .await;
        let event = rx.try_recv().unwrap();
        assert!(
            matches!(event, AppEvent::FavoriteChanged { ids: ref got, is_favorite: true } if got == &ids)
        );
    }

    #[tokio::test]
    async fn create_album_success_without_ids() {
        let lib = MockLibrary::mock();
        let (bus, rx) = EventSender::test_channel();
        handle_command(
            AppEvent::CreateAlbumRequested {
                name: "Vacation".into(),
                ids: vec![],
            },
            &lib,
            &bus,
        )
        .await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::AlbumCreated { ref name, .. } if name == "Vacation"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn create_album_success_with_ids() {
        let lib = MockLibrary::mock();
        let (bus, rx) = EventSender::test_channel();
        handle_command(
            AppEvent::CreateAlbumRequested {
                name: "Trip".into(),
                ids: vec![MediaId::new("photo1".into())],
            },
            &lib,
            &bus,
        )
        .await;
        let event1 = rx.try_recv().unwrap();
        assert!(matches!(event1, AppEvent::AlbumCreated { ref name, .. } if name == "Trip"));
        let event2 = rx.try_recv().unwrap();
        assert!(matches!(event2, AppEvent::AlbumMediaChanged { .. }));
    }

    #[tokio::test]
    async fn create_album_succeeds_but_add_fails() {
        let mock = Arc::new(MockLibrary {
            fail_with: Mutex::new(None),
            items: Mutex::new(Vec::new()),
            fail_add_to_album: Mutex::new(true),
            next_album_id: Mutex::new(AlbumId::new()),
        });
        let lib: Arc<dyn Library> = mock;
        let (bus, rx) = EventSender::test_channel();
        handle_command(
            AppEvent::CreateAlbumRequested {
                name: "Trip".into(),
                ids: vec![MediaId::new("photo1".into())],
            },
            &lib,
            &bus,
        )
        .await;
        let event1 = rx.try_recv().unwrap();
        assert!(matches!(event1, AppEvent::AlbumCreated { ref name, .. } if name == "Trip"));
        let event2 = rx.try_recv().unwrap();
        assert!(matches!(event2, AppEvent::Error(ref msg) if msg.contains("add to album")));
    }

    #[tokio::test]
    async fn empty_trash_with_no_items_is_noop() {
        let lib = MockLibrary::mock();
        let (bus, rx) = EventSender::test_channel();
        handle_command(AppEvent::EmptyTrashRequested, &lib, &bus).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn empty_trash_delete_failure_emits_error() {
        let item = MediaItem {
            id: MediaId::new("abc".into()),
            taken_at: None,
            imported_at: 0,
            original_filename: "test.jpg".into(),
            width: None,
            height: None,
            orientation: 1,
            media_type: MediaType::Image,
            is_favorite: false,
            is_trashed: true,
            trashed_at: Some(0),
            duration_ms: None,
        };
        let lib = MockLibrary::mock_with_items_then_fail(vec![item], "db error");
        let (bus, rx) = EventSender::test_channel();
        handle_command(AppEvent::EmptyTrashRequested, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("empty trash")));
    }

    #[tokio::test]
    async fn restore_all_with_no_items_is_noop() {
        let lib = MockLibrary::mock();
        let (bus, rx) = EventSender::test_channel();
        handle_command(AppEvent::RestoreAllTrashRequested, &lib, &bus).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn restore_all_failure_emits_error() {
        let item = MediaItem {
            id: MediaId::new("abc".into()),
            taken_at: None,
            imported_at: 0,
            original_filename: "test.jpg".into(),
            width: None,
            height: None,
            orientation: 1,
            media_type: MediaType::Image,
            is_favorite: false,
            is_trashed: true,
            trashed_at: Some(0),
            duration_ms: None,
        };
        let lib = MockLibrary::mock_with_items_then_fail(vec![item], "db error");
        let (bus, rx) = EventSender::test_channel();
        handle_command(AppEvent::RestoreAllTrashRequested, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("restore")));
    }
}

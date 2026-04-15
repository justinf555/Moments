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
    library: Arc<Library>,
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
pub(crate) async fn handle_command(event: AppEvent, library: &Arc<Library>, bus: &EventSender) {
    match event {
        AppEvent::TrashRequested { ids } => match library.media().trash(&ids).await {
            Ok(()) => bus.send(AppEvent::Trashed { ids }),
            Err(e) => {
                error!("trash failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to move to trash: {e}")));
            }
        },

        AppEvent::RestoreRequested { ids } => match library.media().restore(&ids).await {
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

        AppEvent::FavoriteRequested { ids, state } => {
            match library.media().set_favorite(&ids, state).await {
                Ok(()) => bus.send(AppEvent::FavoriteChanged {
                    ids,
                    is_favorite: state,
                }),
                Err(e) => {
                    error!("set_favorite failed: {e}");
                    bus.send(AppEvent::Error(format!("Failed to update favourite: {e}")));
                }
            }
        }

        AppEvent::RemoveFromAlbumRequested { album_id, ids } => {
            match library.albums().remove_from_album(&album_id, &ids).await {
                Ok(()) => bus.send(AppEvent::AlbumMediaChanged { album_id }),
                Err(e) => {
                    error!("remove_from_album failed: {e}");
                    bus.send(AppEvent::Error(format!("Failed to remove from album: {e}")));
                }
            }
        }

        AppEvent::AddToAlbumRequested { album_id, ids } => {
            match library.albums().add_to_album(&album_id, &ids).await {
                Ok(()) => bus.send(AppEvent::AlbumMediaChanged { album_id }),
                Err(e) => {
                    error!("add_to_album failed: {e}");
                    bus.send(AppEvent::Error(format!("Failed to add to album: {e}")));
                }
            }
        }

        AppEvent::CreateAlbumRequested { name, ids } => {
            match library.albums().create_album(&name).await {
                Ok(album_id) => {
                    debug!(album_id = %album_id, %name, "album created");
                    bus.send(AppEvent::AlbumCreated {
                        id: album_id.clone(),
                        name,
                    });
                    if !ids.is_empty() {
                        match library.albums().add_to_album(&album_id, &ids).await {
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
            }
        }

        AppEvent::DeleteAlbumRequested { ids } => {
            for id in ids {
                match library.albums().delete_album(&id).await {
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
                .media()
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
                .media()
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
            match library.media().restore(&ids).await {
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
    use crate::library::bundle::Bundle;
    use crate::library::config::{LibraryConfig, LocalStorageMode};
    use crate::library::media::{MediaId, MediaRecord, MediaType};

    async fn test_library() -> Arc<Library> {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(
            &bundle_path,
            &LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            },
        )
        .unwrap();
        std::mem::forget(dir);
        Arc::new(
            Library::open(
                bundle,
                LocalStorageMode::Managed,
                crate::library::db::Database::new(),
                std::sync::Arc::new(crate::sync::outbox::NoOpRecorder),
            )
            .await
            .unwrap(),
        )
    }

    fn test_record(id: MediaId) -> MediaRecord {
        MediaRecord {
            id,
            relative_path: "test.jpg".to_string(),
            original_filename: "test.jpg".to_string(),
            file_size: 1000,
            imported_at: chrono::Utc::now().timestamp(),
            media_type: MediaType::Image,
            taken_at: Some(1000),
            width: Some(100),
            height: Some(100),
            orientation: 1,
            duration_ms: None,
            is_favorite: false,
            is_trashed: false,
            trashed_at: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trash_success_emits_trashed() {
        let lib = test_library().await;
        let id = MediaId::new("a".repeat(64));
        lib.media()
            .insert_media(&test_record(id.clone()))
            .await
            .unwrap();
        let (bus, rx) = EventSender::test_channel();
        handle_command(
            AppEvent::TrashRequested {
                ids: vec![id.clone()],
            },
            &lib,
            &bus,
        )
        .await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Trashed { ids: ref got } if got == &[id]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restore_success_emits_restored() {
        let lib = test_library().await;
        let id = MediaId::new("b".repeat(64));
        lib.media()
            .insert_media(&test_record(id.clone()))
            .await
            .unwrap();
        lib.media().trash(std::slice::from_ref(&id)).await.unwrap();
        let (bus, rx) = EventSender::test_channel();
        handle_command(
            AppEvent::RestoreRequested {
                ids: vec![id.clone()],
            },
            &lib,
            &bus,
        )
        .await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Restored { ids: ref got } if got == &[id]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_success_emits_deleted() {
        let lib = test_library().await;
        let id = MediaId::new("c".repeat(64));
        lib.media()
            .insert_media(&test_record(id.clone()))
            .await
            .unwrap();
        let (bus, rx) = EventSender::test_channel();
        handle_command(
            AppEvent::DeleteRequested {
                ids: vec![id.clone()],
            },
            &lib,
            &bus,
        )
        .await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Deleted { ids: ref got } if got == &[id]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn favorite_success_emits_changed() {
        let lib = test_library().await;
        let id = MediaId::new("d".repeat(64));
        lib.media()
            .insert_media(&test_record(id.clone()))
            .await
            .unwrap();
        let (bus, rx) = EventSender::test_channel();
        handle_command(
            AppEvent::FavoriteRequested {
                ids: vec![id.clone()],
                state: true,
            },
            &lib,
            &bus,
        )
        .await;
        let event = rx.try_recv().unwrap();
        assert!(
            matches!(event, AppEvent::FavoriteChanged { ids: ref got, is_favorite: true } if got == &[id])
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_album_success_without_ids() {
        let lib = test_library().await;
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

    #[tokio::test(flavor = "multi_thread")]
    async fn create_album_success_with_ids() {
        let lib = test_library().await;
        let id = MediaId::new("e".repeat(64));
        lib.media()
            .insert_media(&test_record(id.clone()))
            .await
            .unwrap();
        let (bus, rx) = EventSender::test_channel();
        handle_command(
            AppEvent::CreateAlbumRequested {
                name: "Trip".into(),
                ids: vec![id],
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

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_trash_with_no_items_is_noop() {
        let lib = test_library().await;
        let (bus, rx) = EventSender::test_channel();
        handle_command(AppEvent::EmptyTrashRequested, &lib, &bus).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restore_all_with_no_items_is_noop() {
        let lib = test_library().await;
        let (bus, rx) = EventSender::test_channel();
        handle_command(AppEvent::RestoreAllTrashRequested, &lib, &bus).await;
        assert!(rx.try_recv().is_err());
    }
}

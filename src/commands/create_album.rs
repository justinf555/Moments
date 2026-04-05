use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, error};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

use super::CommandHandler;

pub struct CreateAlbumCommand;

#[async_trait]
impl CommandHandler for CreateAlbumCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::CreateAlbumRequested { .. })
    }

    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &EventSender,
    ) {
        let AppEvent::CreateAlbumRequested { name, ids } = event else { return };
        match library.create_album(&name).await {
            Ok(album_id) => {
                debug!(album_id = %album_id, %name, "album created");
                // AlbumCreated is emitted before the optional add — the album
                // exists regardless of whether the add succeeds. A failed add
                // leaves a valid empty album; the error toast informs the user.
                bus.send(AppEvent::AlbumCreated {
                    id: album_id.clone(),
                    name,
                });
                // Add selected photos to the new album if any.
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_helpers::MockLibrary;
    use crate::library::media::MediaId;

    #[tokio::test]
    async fn handles_create_album_requested() {
        let event = AppEvent::CreateAlbumRequested { name: "Test".into(), ids: vec![] };
        assert!(CreateAlbumCommand.handles(&event));
    }

    #[tokio::test]
    async fn ignores_other_events() {
        assert!(!CreateAlbumCommand.handles(&AppEvent::Ready));
    }

    #[tokio::test]
    async fn success_without_ids_emits_album_created() {
        let lib = MockLibrary::mock();
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        CreateAlbumCommand.execute(
            AppEvent::CreateAlbumRequested { name: "Vacation".into(), ids: vec![] },
            &lib, &bus,
        ).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::AlbumCreated { ref name, .. } if name == "Vacation"));
        // No AlbumMediaChanged since ids was empty.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn success_with_ids_emits_created_and_media_changed() {
        let lib = MockLibrary::mock();
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        CreateAlbumCommand.execute(
            AppEvent::CreateAlbumRequested {
                name: "Trip".into(),
                ids: vec![MediaId::new("photo1".into())],
            },
            &lib, &bus,
        ).await;
        let event1 = rx.try_recv().unwrap();
        assert!(matches!(event1, AppEvent::AlbumCreated { ref name, .. } if name == "Trip"));
        let event2 = rx.try_recv().unwrap();
        assert!(matches!(event2, AppEvent::AlbumMediaChanged { .. }));
    }

    #[tokio::test]
    async fn failure_emits_error() {
        let lib = MockLibrary::mock_failing("db error");
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        CreateAlbumCommand.execute(
            AppEvent::CreateAlbumRequested { name: "Fail".into(), ids: vec![] },
            &lib, &bus,
        ).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("create album")));
    }

    #[tokio::test]
    async fn create_succeeds_but_add_fails_emits_created_then_error() {
        use std::sync::Arc;
        use tokio::sync::Mutex;
        use crate::commands::test_helpers::MockLibrary;
        use crate::library::album::AlbumId;

        let mock = Arc::new(MockLibrary {
            fail_with: Mutex::new(None),
            items: Mutex::new(Vec::new()),
            fail_add_to_album: Mutex::new(true),
            next_album_id: Mutex::new(AlbumId::new()),
        });
        let lib: Arc<dyn crate::library::Library> = mock;
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        CreateAlbumCommand.execute(
            AppEvent::CreateAlbumRequested {
                name: "Trip".into(),
                ids: vec![MediaId::new("photo1".into())],
            },
            &lib, &bus,
        ).await;
        // Album is created first — exists even if add fails.
        let event1 = rx.try_recv().unwrap();
        assert!(matches!(event1, AppEvent::AlbumCreated { ref name, .. } if name == "Trip"));
        // Then the add failure is reported.
        let event2 = rx.try_recv().unwrap();
        assert!(matches!(event2, AppEvent::Error(ref msg) if msg.contains("add to album")));
    }
}

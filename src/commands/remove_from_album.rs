use std::sync::Arc;

use async_trait::async_trait;
use tracing::error;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

use super::CommandHandler;

pub struct RemoveFromAlbumCommand;

#[async_trait]
impl CommandHandler for RemoveFromAlbumCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::RemoveFromAlbumRequested { .. })
    }

    async fn execute(&self, event: AppEvent, library: &Arc<dyn Library>, bus: &EventSender) {
        let AppEvent::RemoveFromAlbumRequested { album_id, ids } = event else {
            return;
        };
        match library.remove_from_album(&album_id, &ids).await {
            Ok(()) => {
                bus.send(AppEvent::AlbumMediaChanged { album_id });
            }
            Err(e) => {
                error!("remove_from_album failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to remove from album: {e}")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_helpers::MockLibrary;
    use crate::library::album::AlbumId;
    use crate::library::media::MediaId;

    #[tokio::test]
    async fn handles_remove_from_album_requested() {
        let event = AppEvent::RemoveFromAlbumRequested {
            album_id: AlbumId::new(),
            ids: vec![],
        };
        assert!(RemoveFromAlbumCommand.handles(&event));
    }

    #[tokio::test]
    async fn ignores_other_events() {
        assert!(!RemoveFromAlbumCommand.handles(&AppEvent::Ready));
    }

    #[tokio::test]
    async fn success_emits_album_media_changed() {
        let lib = MockLibrary::mock();
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        let album_id = AlbumId::new();
        RemoveFromAlbumCommand
            .execute(
                AppEvent::RemoveFromAlbumRequested {
                    album_id: album_id.clone(),
                    ids: vec![MediaId::new("a".into())],
                },
                &lib,
                &bus,
            )
            .await;
        let event = rx.try_recv().unwrap();
        assert!(
            matches!(event, AppEvent::AlbumMediaChanged { album_id: ref got } if got == &album_id)
        );
    }

    #[tokio::test]
    async fn failure_emits_error() {
        let lib = MockLibrary::mock_failing("db error");
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        RemoveFromAlbumCommand
            .execute(
                AppEvent::RemoveFromAlbumRequested {
                    album_id: AlbumId::new(),
                    ids: vec![],
                },
                &lib,
                &bus,
            )
            .await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("remove from album")));
    }
}

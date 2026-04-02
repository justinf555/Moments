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

    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &EventSender,
    ) {
        let AppEvent::RemoveFromAlbumRequested { album_id, ids } = event else { return };
        match library.remove_from_album(&album_id, &ids).await {
            Ok(()) => {
                bus.send(AppEvent::AlbumMediaChanged {
                    album_id,
                });
            }
            Err(e) => {
                error!("remove_from_album failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to remove from album: {e}")));
            }
        }
    }
}

use std::sync::Arc;

use async_trait::async_trait;
use tracing::error;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

use super::CommandHandler;

pub struct AddToAlbumCommand;

#[async_trait]
impl CommandHandler for AddToAlbumCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::AddToAlbumRequested { .. })
    }

    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &EventSender,
    ) {
        let AppEvent::AddToAlbumRequested { album_id, ids } = event else { return };
        match library.add_to_album(&album_id, &ids).await {
            Ok(()) => {
                bus.send(AppEvent::AlbumMediaChanged { album_id });
            }
            Err(e) => {
                error!("add_to_album failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to add to album: {e}")));
            }
        }
    }
}

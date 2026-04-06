use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, error};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

use super::CommandHandler;

pub struct DeleteAlbumCommand;

#[async_trait]
impl CommandHandler for DeleteAlbumCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::DeleteAlbumRequested { .. })
    }

    async fn execute(&self, event: AppEvent, library: &Arc<dyn Library>, bus: &EventSender) {
        let AppEvent::DeleteAlbumRequested { ids } = event else {
            return;
        };
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
}

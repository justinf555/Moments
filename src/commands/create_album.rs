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

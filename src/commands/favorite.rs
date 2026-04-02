use std::sync::Arc;

use async_trait::async_trait;
use tracing::error;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

use super::CommandHandler;

pub struct FavoriteCommand;

#[async_trait]
impl CommandHandler for FavoriteCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::FavoriteRequested { .. })
    }

    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &EventSender,
    ) {
        let AppEvent::FavoriteRequested { ids, state } = event else { return };
        match library.set_favorite(&ids, state).await {
            Ok(()) => {
                bus.send(AppEvent::FavoriteChanged {
                    ids,
                    is_favorite: state,
                });
            }
            Err(e) => {
                error!("set_favorite failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to update favourite: {e}")));
            }
        }
    }
}

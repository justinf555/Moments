use std::sync::Arc;

use async_trait::async_trait;
use tracing::error;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

use super::CommandHandler;

pub struct TrashCommand;

#[async_trait]
impl CommandHandler for TrashCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::TrashRequested { .. })
    }

    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &EventSender,
    ) {
        let AppEvent::TrashRequested { ids } = event else { return };
        match library.trash(&ids).await {
            Ok(()) => {
                bus.send(AppEvent::Trashed { ids });
            }
            Err(e) => {
                error!("trash failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to move to trash: {e}")));
            }
        }
    }
}

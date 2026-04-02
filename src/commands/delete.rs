use std::sync::Arc;

use async_trait::async_trait;
use tracing::error;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

use super::CommandHandler;

pub struct DeleteCommand;

#[async_trait]
impl CommandHandler for DeleteCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::DeleteRequested { .. })
    }

    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &EventSender,
    ) {
        let AppEvent::DeleteRequested { ids } = event else { return };
        match library.delete_permanently(&ids).await {
            Ok(()) => {
                bus.send(AppEvent::Deleted { ids });
            }
            Err(e) => {
                error!("delete permanently failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to delete: {e}")));
            }
        }
    }
}

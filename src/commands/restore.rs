use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, error};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

use super::CommandHandler;

pub struct RestoreCommand;

#[async_trait]
impl CommandHandler for RestoreCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::RestoreRequested { .. })
    }

    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &EventSender,
    ) {
        let AppEvent::RestoreRequested { ids } = event else { return };
        debug!(count = ids.len(), "RestoreCommand: restoring items");
        match library.restore(&ids).await {
            Ok(()) => {
                debug!(count = ids.len(), "RestoreCommand: success, sending Restored event");
                bus.send(AppEvent::Restored { ids });
            }
            Err(e) => {
                error!("restore failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to restore: {e}")));
            }
        }
    }
}

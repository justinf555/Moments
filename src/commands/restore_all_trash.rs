use std::sync::Arc;

use async_trait::async_trait;
use tracing::{error, info};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::media::MediaFilter;
use crate::library::Library;

use super::CommandHandler;

pub struct RestoreAllTrashCommand;

#[async_trait]
impl CommandHandler for RestoreAllTrashCommand {
    fn handles(&self, event: &AppEvent) -> bool {
        matches!(event, AppEvent::RestoreAllTrashRequested)
    }

    async fn execute(
        &self,
        _event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &EventSender,
    ) {
        let items = match library.list_media(MediaFilter::Trashed, None, u32::MAX).await {
            Ok(items) => items,
            Err(e) => {
                error!("failed to list trashed items: {e}");
                bus.send(AppEvent::Error(format!("Failed to restore trash: {e}")));
                return;
            }
        };

        if items.is_empty() {
            return;
        }

        let ids: Vec<_> = items.into_iter().map(|i| i.id).collect();
        let count = ids.len();

        match library.restore(&ids).await {
            Ok(()) => {
                info!(count, "all trash restored");
                bus.send(AppEvent::Restored { ids });
            }
            Err(e) => {
                error!("restore all trash failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to restore: {e}")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_helpers::MockLibrary;

    #[tokio::test]
    async fn handles_restore_all_trash_requested() {
        assert!(RestoreAllTrashCommand.handles(&AppEvent::RestoreAllTrashRequested));
    }

    #[tokio::test]
    async fn ignores_other_events() {
        assert!(!RestoreAllTrashCommand.handles(&AppEvent::Ready));
    }

    #[tokio::test]
    async fn restore_all_with_no_items_is_noop() {
        let lib = MockLibrary::mock();
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        RestoreAllTrashCommand.execute(AppEvent::RestoreAllTrashRequested, &lib, &bus).await;
        // No items in trash → no events emitted.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn failure_emits_error() {
        let lib = MockLibrary::mock_failing("db error");
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        RestoreAllTrashCommand.execute(AppEvent::RestoreAllTrashRequested, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("restore trash")));
    }
}

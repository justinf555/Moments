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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_helpers::MockLibrary;
    use crate::library::media::MediaId;

    #[tokio::test]
    async fn handles_delete_requested() {
        assert!(DeleteCommand.handles(&AppEvent::DeleteRequested { ids: vec![] }));
    }

    #[tokio::test]
    async fn ignores_other_events() {
        assert!(!DeleteCommand.handles(&AppEvent::Ready));
    }

    #[tokio::test]
    async fn success_emits_deleted() {
        let lib = MockLibrary::mock();
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        let ids = vec![MediaId::new("abc".into())];
        DeleteCommand.execute(AppEvent::DeleteRequested { ids: ids.clone() }, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Deleted { ids: ref got } if got == &ids));
    }

    #[tokio::test]
    async fn failure_emits_error() {
        let lib = MockLibrary::mock_failing("db error");
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        DeleteCommand.execute(AppEvent::DeleteRequested { ids: vec![MediaId::new("x".into())] }, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("delete")));
    }
}

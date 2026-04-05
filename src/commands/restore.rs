use std::sync::Arc;

use async_trait::async_trait;
use tracing::error;

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
        match library.restore(&ids).await {
            Ok(()) => {
                bus.send(AppEvent::Restored { ids });
            }
            Err(e) => {
                error!("restore failed: {e}");
                bus.send(AppEvent::Error(format!("Failed to restore: {e}")));
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
    async fn handles_restore_requested() {
        assert!(RestoreCommand.handles(&AppEvent::RestoreRequested { ids: vec![] }));
    }

    #[tokio::test]
    async fn ignores_other_events() {
        assert!(!RestoreCommand.handles(&AppEvent::Ready));
    }

    #[tokio::test]
    async fn success_emits_restored() {
        let lib = MockLibrary::mock();
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        let ids = vec![MediaId::new("abc".into())];
        RestoreCommand.execute(AppEvent::RestoreRequested { ids: ids.clone() }, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Restored { ids: ref got } if got == &ids));
    }

    #[tokio::test]
    async fn failure_emits_error() {
        let lib = MockLibrary::mock_failing("db error");
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        RestoreCommand.execute(AppEvent::RestoreRequested { ids: vec![MediaId::new("x".into())] }, &lib, &bus).await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("restore")));
    }
}

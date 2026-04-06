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

    async fn execute(&self, event: AppEvent, library: &Arc<dyn Library>, bus: &EventSender) {
        let AppEvent::TrashRequested { ids } = event else {
            return;
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_helpers::MockLibrary;
    use crate::library::media::MediaId;

    #[tokio::test]
    async fn handles_trash_requested() {
        let cmd = TrashCommand;
        let event = AppEvent::TrashRequested { ids: vec![] };
        assert!(cmd.handles(&event));
    }

    #[tokio::test]
    async fn ignores_other_events() {
        let cmd = TrashCommand;
        assert!(!cmd.handles(&AppEvent::Ready));
    }

    #[tokio::test]
    async fn success_emits_trashed() {
        let cmd = TrashCommand;
        let lib = MockLibrary::mock();
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        let ids = vec![MediaId::new("abc".into())];
        cmd.execute(AppEvent::TrashRequested { ids: ids.clone() }, &lib, &bus)
            .await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Trashed { ids: ref got } if got == &ids));
    }

    #[tokio::test]
    async fn failure_emits_error() {
        let cmd = TrashCommand;
        let lib = MockLibrary::mock_failing("db error");
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        cmd.execute(
            AppEvent::TrashRequested {
                ids: vec![MediaId::new("x".into())],
            },
            &lib,
            &bus,
        )
        .await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("trash")));
    }
}

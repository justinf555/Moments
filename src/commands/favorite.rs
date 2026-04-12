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

    async fn execute(&self, event: AppEvent, library: &Arc<dyn Library>, bus: &EventSender) {
        let AppEvent::FavoriteRequested { ids, state } = event else {
            return;
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_helpers::MockLibrary;
    use crate::library::media::MediaId;

    #[tokio::test]
    async fn handles_favorite_requested() {
        assert!(FavoriteCommand.handles(&AppEvent::FavoriteRequested {
            ids: vec![],
            state: true
        }));
    }

    #[tokio::test]
    async fn ignores_other_events() {
        assert!(!FavoriteCommand.handles(&AppEvent::Ready));
    }

    #[tokio::test]
    async fn success_emits_favorite_changed() {
        let lib = MockLibrary::mock();
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        let ids = vec![MediaId::new("abc".into())];
        FavoriteCommand
            .execute(
                AppEvent::FavoriteRequested {
                    ids: ids.clone(),
                    state: true,
                },
                &lib,
                &bus,
            )
            .await;
        let event = rx.try_recv().unwrap();
        assert!(
            matches!(event, AppEvent::FavoriteChanged { ids: ref got, is_favorite: true } if got == &ids)
        );
    }

    #[tokio::test]
    async fn failure_emits_error() {
        let lib = MockLibrary::mock_failing("db error");
        let (bus, rx) = crate::event_bus::EventSender::test_channel();
        FavoriteCommand
            .execute(
                AppEvent::FavoriteRequested {
                    ids: vec![MediaId::new("x".into())],
                    state: false,
                },
                &lib,
                &bus,
            )
            .await;
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, AppEvent::Error(msg) if msg.contains("favourite")));
    }
}

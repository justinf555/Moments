pub mod add_to_album;
pub mod create_album;
pub mod delete;
pub mod dispatcher;
pub mod favorite;
pub mod remove_from_album;
pub mod restore;
pub mod trash;

use std::sync::Arc;

use async_trait::async_trait;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;

/// Trait for a single command handler.
///
/// Each command is its own struct implementing this trait. The
/// [`CommandDispatcher`](dispatcher::CommandDispatcher) routes events to
/// the handler that claims them via [`handles`](Self::handles).
///
/// Handlers execute on the Tokio runtime (not the GTK thread) and send
/// result events back via the bus sender.
///
/// See `docs/design-event-bus.md` for the full design.
#[async_trait]
pub trait CommandHandler: Send + Sync {
    /// Returns true if this handler can process the given event.
    fn handles(&self, event: &AppEvent) -> bool;

    /// Execute the command on the Tokio runtime.
    ///
    /// On success, sends the result event via the bus sender.
    /// On failure, sends `AppEvent::Error` with a user-facing message.
    async fn execute(
        &self,
        event: AppEvent,
        library: &Arc<dyn Library>,
        bus: &EventSender,
    );
}

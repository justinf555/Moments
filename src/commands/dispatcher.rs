use std::sync::Arc;

use crate::event_bus::EventBus;
use crate::library::Library;

use super::add_to_album::AddToAlbumCommand;
use super::create_album::CreateAlbumCommand;
use super::delete::DeleteCommand;
use super::delete_album::DeleteAlbumCommand;
use super::empty_trash::EmptyTrashCommand;
use super::favorite::FavoriteCommand;
use super::remove_from_album::RemoveFromAlbumCommand;
use super::restore::RestoreCommand;
use super::restore_all_trash::RestoreAllTrashCommand;
use super::trash::TrashCommand;
use super::CommandHandler;

/// Routes command events to their handlers on the Tokio runtime.
///
/// Subscribes to the event bus and dispatches each `*Requested` event to
/// the handler that claims it. Each command is spawned as an independent
/// Tokio task — concurrent, not sequential.
///
/// `library` and `tokio` exist in exactly one place — here. No other
/// component needs them for action execution.
///
/// Holds a [`Subscription`] — the subscriber is removed when the
/// dispatcher is dropped.
pub struct CommandDispatcher {
    _subscription: crate::event_bus::Subscription,
}

impl CommandDispatcher {
    pub fn new(library: Arc<dyn Library>, tokio: tokio::runtime::Handle, bus: &EventBus) -> Self {
        let handlers: Vec<Arc<dyn CommandHandler>> = vec![
            Arc::new(TrashCommand),
            Arc::new(RestoreCommand),
            Arc::new(DeleteCommand),
            Arc::new(FavoriteCommand),
            Arc::new(RemoveFromAlbumCommand),
            Arc::new(AddToAlbumCommand),
            Arc::new(CreateAlbumCommand),
            Arc::new(DeleteAlbumCommand),
            Arc::new(EmptyTrashCommand),
            Arc::new(RestoreAllTrashCommand),
        ];

        let tx = bus.sender();

        let subscription = bus.subscribe(move |event| {
            for handler in &handlers {
                if handler.handles(event) {
                    let h = Arc::clone(handler);
                    let lib = Arc::clone(&library);
                    let bus_tx = tx.clone();
                    let evt = event.clone();
                    tokio.spawn(async move {
                        h.execute(evt, &lib, &bus_tx).await;
                    });
                    break;
                }
            }
        });

        Self {
            _subscription: subscription,
        }
    }
}

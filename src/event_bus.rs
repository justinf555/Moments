use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

use gtk::glib;

use crate::app_event::AppEvent;

/// Centralised event bus with fan-out subscriber delivery.
///
/// The bus owns a `std::sync::mpsc` channel. The [`sender`](Self::sender)
/// is `Send + Clone` so it can be used from Tokio tasks (library events,
/// command handlers). A GTK-thread poll source drains the channel and
/// dispatches each event to all registered subscribers.
///
/// Subscribers are registered via [`subscribe`](Self::subscribe) and receive
/// every event — they filter internally with a `match`. This keeps the bus
/// simple and avoids per-event-type subscription machinery.
///
/// See `docs/design-event-bus.md` for the full design.
pub struct EventBus {
    tx: mpsc::Sender<AppEvent>,
    subscribers: Rc<RefCell<Vec<Box<dyn Fn(&AppEvent)>>>>,
    _source_id: glib::SourceId,
}

impl EventBus {
    /// Create a new event bus and start the GTK-thread drain source.
    ///
    /// Must be called on the GTK main thread.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<AppEvent>();
        let subscribers: Rc<RefCell<Vec<Box<dyn Fn(&AppEvent)>>>> =
            Rc::new(RefCell::new(Vec::new()));

        // Poll the channel every 4ms and fan out to subscribers.
        // This matches the current idle-loop pattern. A custom GLib Source
        // could eliminate polling entirely, but 4ms is responsive enough
        // for UI events (button clicks, thumbnail arrivals).
        let subs = Rc::clone(&subscribers);
        let source_id = glib::timeout_add_local(
            std::time::Duration::from_millis(4),
            move || {
                while let Ok(event) = rx.try_recv() {
                    for handler in subs.borrow().iter() {
                        handler(&event);
                    }
                }
                glib::ControlFlow::Continue
            },
        );

        Self {
            tx,
            subscribers,
            _source_id: source_id,
        }
    }

    /// Get a sender for producing events.
    ///
    /// The sender is `Send + Clone` — safe to use from Tokio tasks,
    /// background threads, and GTK signal handlers.
    pub fn sender(&self) -> mpsc::Sender<AppEvent> {
        self.tx.clone()
    }

    /// Register a subscriber callback. Called on the GTK main thread.
    ///
    /// The subscriber receives every event — use `match` to filter.
    /// Subscribers are called in registration order.
    pub fn subscribe(&self, handler: impl Fn(&AppEvent) + 'static) {
        self.subscribers.borrow_mut().push(Box::new(handler));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_is_send_and_clone() {
        fn assert_send<T: Send>() {}
        fn assert_clone<T: Clone>() {}
        assert_send::<mpsc::Sender<AppEvent>>();
        assert_clone::<mpsc::Sender<AppEvent>>();
    }
}

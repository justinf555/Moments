use std::cell::RefCell;
use std::sync::mpsc;

use gtk::glib;

use crate::app_event::AppEvent;

/// Centralised event bus with push-based fan-out subscriber delivery.
///
/// Events are delivered to all subscribers on the GTK main thread via
/// `glib::idle_add_once` — zero CPU when idle, zero polling, no timer.
///
/// The [`sender`](Self::sender) is `Send + Clone` so it can be used from
/// Tokio tasks (library events, command handlers). Each `send()` schedules
/// a main-loop callback that drains the internal channel and dispatches to
/// all registered subscribers.
///
/// # Subscriber contract
///
/// All subscribers must be registered before events begin flowing (i.e. in
/// component constructors during app setup). Calling [`subscribe`](Self::subscribe)
/// from within a subscriber callback will panic due to `RefCell` re-entrancy.
///
/// See `docs/design-event-bus.md` for the full design.
pub struct EventBus {
    tx: mpsc::Sender<AppEvent>,
}

// Thread-local subscriber list. Accessible from `idle_add_once` callbacks
// which run on the GTK main thread. This avoids the Send constraint — the
// subscriber closures capture widget references (Rc, not Send).
thread_local! {
    static SUBSCRIBERS: RefCell<Vec<Box<dyn Fn(&AppEvent)>>> = const { RefCell::new(Vec::new()) };
    static RECEIVER: RefCell<Option<mpsc::Receiver<AppEvent>>> = const { RefCell::new(None) };
}

/// Drain all pending events from the channel and deliver to subscribers.
///
/// Called via `glib::idle_add_once` — runs exactly once per `send()` call,
/// but drains all accumulated events (handles burst sends).
fn drain_events() {
    RECEIVER.with(|rx_cell| {
        let rx = rx_cell.borrow();
        let Some(rx) = rx.as_ref() else { return };

        SUBSCRIBERS.with(|subs_cell| {
            let subs = subs_cell.borrow();
            // NOTE: subscribers must not call sender.send() from within a handler.
            // Events sent during dispatch are picked up by this same while loop —
            // any cycle (A emits B, B emits A) will loop forever and hang the UI.
            // See the circular event loop risk in docs/design-event-bus.md.
            while let Ok(event) = rx.try_recv() {
                for handler in subs.iter() {
                    handler(&event);
                }
            }
        });
    });
}

impl EventBus {
    /// Create a new event bus.
    ///
    /// Must be called on the GTK main thread. Only one `EventBus` may exist
    /// per thread (the subscriber list is thread-local).
    pub fn new() -> Self {
        RECEIVER.with(|cell| {
            assert!(
                cell.borrow().is_none(),
                "EventBus: only one instance per thread is allowed"
            );
        });

        let (tx, rx) = mpsc::channel::<AppEvent>();

        RECEIVER.with(|cell| {
            *cell.borrow_mut() = Some(rx);
        });

        Self { tx }
    }

    /// Get a sender for producing events.
    ///
    /// The sender is `Send + Clone` — safe to use from Tokio tasks,
    /// background threads, and GTK signal handlers. Each `send()` wakes
    /// the GLib main loop to process the event (push-based, no polling).
    pub fn sender(&self) -> EventSender {
        EventSender {
            tx: self.tx.clone(),
        }
    }

    /// Register a subscriber callback. Called on the GTK main thread.
    ///
    /// The subscriber receives every event — use `match` to filter.
    /// Subscribers are called in registration order.
    ///
    /// **Must not be called from within a subscriber callback** — the
    /// `RefCell` borrow will panic. Register all subscribers during
    /// component construction, before events start flowing.
    pub fn subscribe(&self, handler: impl Fn(&AppEvent) + 'static) {
        SUBSCRIBERS.with(|cell| {
            cell.borrow_mut().push(Box::new(handler));
        });
    }
}

/// Subscribe to the event bus from any code running on the GTK main thread.
///
/// This is a convenience for components created lazily (e.g. in `register_lazy`
/// closures) that don't have a direct reference to the `EventBus` struct.
/// Equivalent to calling `bus.subscribe(handler)`.
pub fn subscribe(handler: impl Fn(&AppEvent) + 'static) {
    SUBSCRIBERS.with(|cell| {
        cell.borrow_mut().push(Box::new(handler));
    });
}

impl Drop for EventBus {
    fn drop(&mut self) {
        // Clear the thread-local state so a new EventBus can be created
        // (e.g. in tests where each test creates its own bus).
        RECEIVER.with(|cell| {
            cell.borrow_mut().take();
        });
        SUBSCRIBERS.with(|cell| {
            cell.borrow_mut().clear();
        });
    }
}

/// Thread-safe event sender. Cloneable, `Send`.
///
/// Each `send()` pushes the event into an mpsc channel and schedules a
/// `glib::idle_add_once` to drain it on the GTK main thread. This is
/// push-based: the main loop wakes immediately, no polling timer.
#[derive(Clone)]
pub struct EventSender {
    tx: mpsc::Sender<AppEvent>,
}

impl EventSender {
    /// Send an event. Safe to call from any thread.
    ///
    /// The event is delivered to all subscribers on the next GTK main
    /// loop iteration (via `glib::idle_add_once`).
    pub fn send(&self, event: AppEvent) {
        if self.tx.send(event).is_ok() {
            // Wake the main loop to drain the channel.
            // idle_add_once is Send — safe from Tokio threads.
            //
            // NOTE: during burst sends (e.g. 200 thumbnails), each send()
            // schedules a drain callback. The first drains all events; the
            // rest are no-ops. Functionally correct but adds idle-source
            // queue churn proportional to event volume.
            glib::idle_add_once(drain_events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_is_send_and_clone() {
        fn assert_send<T: Send>() {}
        fn assert_clone<T: Clone>() {}
        assert_send::<EventSender>();
        assert_clone::<EventSender>();
    }
}

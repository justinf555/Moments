use std::cell::{Cell, RefCell};
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
/// Subscriptions can be created and dropped at any time, including during
/// event dispatch (e.g. from `WidgetImpl::realize` / `unrealize`).
/// Drops during dispatch are deferred and flushed after the dispatch loop.
///
/// See `docs/design-event-bus.md` for the full design.
pub struct EventBus {
    tx: mpsc::Sender<AppEvent>,
}

struct SubscriberEntry {
    id: u64,
    handler: Box<dyn Fn(&AppEvent)>,
}

type SubscriberList = Vec<SubscriberEntry>;

// Thread-local subscriber list. Accessible from `idle_add_once` callbacks
// which run on the GTK main thread. This avoids the Send constraint — the
// subscriber closures capture widget references (Rc, not Send).
thread_local! {
    static SUBSCRIBERS: RefCell<SubscriberList> = const { RefCell::new(Vec::new()) };
    static RECEIVER: RefCell<Option<mpsc::Receiver<AppEvent>>> = const { RefCell::new(None) };
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
    /// IDs of subscriptions dropped during dispatch. Flushed after the
    /// dispatch loop releases its immutable borrow of `SUBSCRIBERS`.
    static PENDING_REMOVALS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

fn next_subscriber_id() -> u64 {
    NEXT_ID.with(|cell| {
        let id = cell.get();
        cell.set(id + 1);
        id
    })
}

/// RAII handle for an event bus subscription. Removing the subscriber
/// closure from the bus when dropped prevents unbounded growth of the
/// subscriber list over long sessions.
///
/// # Thread safety
///
/// `Subscription` is `!Send` because its `Drop` impl operates on thread-local
/// state. It must be dropped on the same thread that created it (the GTK main
/// thread).
///
/// # Re-entrancy
///
/// Safe to drop during event dispatch (e.g. from a `WidgetImpl::unrealize`
/// triggered by a handler). The removal is deferred and flushed after
/// dispatch completes.
pub struct Subscription {
    id: u64,
    /// Marker to prevent `Send` — `Drop` operates on thread-local state.
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        PENDING_REMOVALS.with(|cell| {
            cell.borrow_mut().push(self.id);
        });
        flush_pending_removals();
    }
}

/// Apply any deferred subscription removals. Uses `try_borrow_mut` so it
/// is a no-op when `drain_events` holds an immutable borrow — the removals
/// are flushed after the dispatch loop instead.
///
/// Drains `PENDING_REMOVALS` to a local vec before touching `SUBSCRIBERS`,
/// so that dropping a `SubscriberEntry` whose closure captures a
/// `Subscription` cannot re-enter `PENDING_REMOVALS.borrow_mut()`.
fn flush_pending_removals() {
    let removals: Vec<u64> = PENDING_REMOVALS.with(|cell| {
        let mut r = cell.borrow_mut();
        if r.is_empty() {
            return vec![];
        }
        r.drain(..).collect()
    });
    if removals.is_empty() {
        return;
    }
    SUBSCRIBERS.with(|subs_cell| {
        if let Ok(mut subs) = subs_cell.try_borrow_mut() {
            subs.retain(|entry| !removals.contains(&entry.id));
        } else {
            // Re-queue: we are inside dispatch, drain_events will flush after.
            PENDING_REMOVALS.with(|cell| cell.borrow_mut().extend(removals));
        }
    });
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
                for entry in subs.iter() {
                    (entry.handler)(&event);
                }
            }
        });

        // Flush any subscriptions that were dropped during dispatch
        // (e.g. via WidgetImpl::unrealize triggered by a handler).
        flush_pending_removals();
    });
}

#[allow(clippy::new_without_default)]
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
    /// Returns a [`Subscription`] handle — the closure is removed when the
    /// handle is dropped. Store it in the subscribing component's state.
    ///
    /// The subscriber receives every event — use `match` to filter.
    /// Subscribers are called in registration order.
    pub fn subscribe(&self, handler: impl Fn(&AppEvent) + 'static) -> Subscription {
        let id = next_subscriber_id();
        SUBSCRIBERS.with(|cell| {
            cell.borrow_mut().push(SubscriberEntry {
                id,
                handler: Box::new(handler),
            });
        });
        Subscription {
            id,
            _not_send: std::marker::PhantomData,
        }
    }
}

/// Subscribe to the event bus from any code running on the GTK main thread.
///
/// This is a convenience for components created lazily (e.g. in `register_lazy`
/// closures) that don't have a direct reference to the `EventBus` struct.
/// Equivalent to calling `bus.subscribe(handler)`.
pub fn subscribe(handler: impl Fn(&AppEvent) + 'static) -> Subscription {
    let id = next_subscriber_id();
    SUBSCRIBERS.with(|cell| {
        cell.borrow_mut().push(SubscriberEntry {
            id,
            handler: Box::new(handler),
        });
    });
    Subscription {
        id,
        _not_send: std::marker::PhantomData,
    }
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
        PENDING_REMOVALS.with(|cell| {
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
    /// Create a no-op sender for testing. Events are sent but never drained.
    pub fn no_op() -> Self {
        let (tx, _rx) = mpsc::channel();
        Self { tx }
    }

    /// Create a sender + receiver pair for unit tests.
    ///
    /// Unlike [`no_op`](Self::no_op), the receiver is returned so tests can
    /// assert on emitted events. The `glib::idle_add_once` call in `send()`
    /// still fires but is harmless in `#[tokio::test]` (no GTK main loop to
    /// wake — the idle source is simply never dispatched).
    #[cfg(test)]
    pub fn test_channel() -> (Self, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = mpsc::channel();
        (Self { tx }, rx)
    }

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

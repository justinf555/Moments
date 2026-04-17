//! Multi-subscriber event fan-out primitive used by library services.
//!
//! Each service holds one `EventEmitter<Event>` as a private field. The
//! service's own `subscribe()` and `emit()` methods delegate here so the
//! pattern is identical across Album, Media, Faces, and Thumbnail services.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

/// Multi-subscriber wrapper around `tokio::sync::mpsc::UnboundedSender`.
///
/// [`subscribe`] hands out a fresh `UnboundedReceiver` per call; [`emit`]
/// delivers a clone of the event to every live subscriber. Senders whose
/// receiver has been dropped are pruned on the next emit.
///
/// The sender list is `Arc`-wrapped so [`Clone`]d copies of the emitter
/// (e.g. inside a `#[derive(Clone)]` service) share the same subscriber set:
/// a subscriber attached on one clone receives events emitted through any
/// clone.
///
/// [`subscribe`]: EventEmitter::subscribe
/// [`emit`]: EventEmitter::emit
pub struct EventEmitter<T: Clone> {
    senders: Arc<Mutex<Vec<mpsc::UnboundedSender<T>>>>,
}

impl<T: Clone> EventEmitter<T> {
    pub fn new() -> Self {
        Self {
            senders: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register a new subscriber and return its receiver.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<T> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.senders
            .lock()
            .expect("event_emitter mutex poisoned")
            .push(tx);
        rx
    }

    /// Broadcast `event` to every live subscriber.
    ///
    /// Senders whose receiver has been dropped are removed in the same pass.
    pub fn emit(&self, event: T) {
        let mut senders = self.senders.lock().expect("event_emitter mutex poisoned");
        senders.retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// Current subscriber count. Diagnostic/test use only.
    #[cfg(test)]
    pub fn subscriber_count(&self) -> usize {
        self.senders
            .lock()
            .expect("event_emitter mutex poisoned")
            .len()
    }
}

impl<T: Clone> Default for EventEmitter<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Clone for EventEmitter<T> {
    fn clone(&self) -> Self {
        Self {
            senders: Arc::clone(&self.senders),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn single_subscriber_receives_event() {
        let em: EventEmitter<u32> = EventEmitter::new();
        let mut rx = em.subscribe();
        em.emit(42);
        assert_eq!(rx.recv().await, Some(42));
    }

    #[tokio::test]
    async fn two_subscribers_both_receive_each_event() {
        let em: EventEmitter<String> = EventEmitter::new();
        let mut rx1 = em.subscribe();
        let mut rx2 = em.subscribe();
        em.emit("hello".to_string());
        em.emit("world".to_string());
        assert_eq!(rx1.recv().await, Some("hello".to_string()));
        assert_eq!(rx1.recv().await, Some("world".to_string()));
        assert_eq!(rx2.recv().await, Some("hello".to_string()));
        assert_eq!(rx2.recv().await, Some("world".to_string()));
    }

    #[tokio::test]
    async fn dropped_receiver_is_pruned_on_next_emit() {
        let em: EventEmitter<u32> = EventEmitter::new();
        let rx1 = em.subscribe();
        let mut rx2 = em.subscribe();
        drop(rx1);
        em.emit(1);
        assert_eq!(em.subscriber_count(), 1);
        assert_eq!(rx2.recv().await, Some(1));
    }

    #[tokio::test]
    async fn emit_with_no_subscribers_is_noop() {
        let em: EventEmitter<u32> = EventEmitter::new();
        em.emit(1);
        em.emit(2);
        assert_eq!(em.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn clone_shares_subscriber_list() {
        let em: EventEmitter<u32> = EventEmitter::new();
        let twin = em.clone();
        let mut rx = em.subscribe();
        twin.emit(7);
        assert_eq!(rx.recv().await, Some(7));
    }
}

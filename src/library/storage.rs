use std::sync::mpsc::Sender;

use async_trait::async_trait;

use super::bundle::Bundle;
use super::error::LibraryError;
use super::event::LibraryEvent;

/// Low-level storage interface implemented by every library backend.
///
/// Backends receive an already-validated [`Bundle`] and a
/// `Sender<LibraryEvent>` at construction time. They store the sender
/// internally and use it to push events to the GTK layer throughout their
/// lifetime.
///
/// `close()` is the single shutdown point — [`crate::library::Library`]
/// inherits it via the supertrait relationship and does not duplicate it.
#[async_trait]
pub trait LibraryStorage: Send + Sync + 'static {
    /// Initialise this backend against an open `bundle`, storing `events`
    /// for the lifetime of the backend.
    ///
    /// Implementations should send [`LibraryEvent::Ready`] once initialisation
    /// is complete.
    async fn open(bundle: Bundle, events: Sender<LibraryEvent>) -> Result<Self, LibraryError>
    where
        Self: Sized;

    /// Gracefully shut down the backend.
    ///
    /// Flushes pending writes and signals background workers to stop after
    /// finishing their current item. Sends [`LibraryEvent::ShutdownComplete`]
    /// before returning.
    async fn close(&self) -> Result<(), LibraryError>;
}

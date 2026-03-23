use std::sync::mpsc::Sender;

use async_trait::async_trait;
use tokio::runtime::Handle;

use super::bundle::Bundle;
use super::error::LibraryError;
use super::event::LibraryEvent;

/// Low-level storage interface implemented by every library backend.
///
/// Backends receive an already-validated [`Bundle`], a `Sender<LibraryEvent>`,
/// and a Tokio [`Handle`] at construction time. The handle is the shared
/// library executor — the same runtime used by all backends, created in
/// `main()` and owned by `MomentsApplication`.
///
/// `close()` is the single shutdown point — [`crate::library::Library`]
/// inherits it via the supertrait relationship and does not duplicate it.
#[async_trait]
pub trait LibraryStorage: Send + Sync + 'static {
    /// Initialise this backend against an open `bundle`, storing `events`
    /// and `tokio` for the lifetime of the backend.
    ///
    /// `tokio` is the application-level Tokio runtime handle. All async
    /// backend work — database queries, file I/O, future HTTP calls — is
    /// dispatched through it, keeping all library work off the GTK thread.
    ///
    /// Implementations should send [`LibraryEvent::Ready`] once initialisation
    /// is complete.
    async fn open(
        bundle: Bundle,
        events: Sender<LibraryEvent>,
        tokio: Handle,
    ) -> Result<Self, LibraryError>
    where
        Self: Sized;

    /// Gracefully shut down the backend.
    ///
    /// Flushes pending writes and signals background workers to stop after
    /// finishing their current item. Sends [`LibraryEvent::ShutdownComplete`]
    /// before returning.
    async fn close(&self) -> Result<(), LibraryError>;
}

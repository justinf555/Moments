pub mod config;
pub mod error;
pub mod event;
pub mod factory;
pub mod storage;
pub mod types;

pub use error::LibraryError;
pub use event::LibraryEvent;

use async_trait::async_trait;

/// The public interface for a Moments library backend.
///
/// The GTK application holds a `Box<dyn Library>` and calls methods on it
/// directly. It never imports or references concrete backend types — those are
/// constructed once by [`factory::LibraryFactory`] and then erased behind this
/// trait.
///
/// Async events flowing back to the GTK layer (e.g. [`LibraryEvent::AssetImported`])
/// are delivered via a `std::sync::mpsc::Sender<LibraryEvent>` that is injected
/// at construction time by `LibraryFactory::create`.
///
/// # Implementing this trait
/// - Store the `Sender<LibraryEvent>` received at construction and use it to
///   push events from background tasks.
/// - Never import GTK or adw types.
/// - Instrument all method implementations with `#[instrument]`.
#[async_trait]
pub trait Library: Send + Sync + 'static {
    /// Gracefully shut down the library.
    ///
    /// Flushes pending writes and signals all background workers to stop after
    /// finishing their current item. Sends [`LibraryEvent::ShutdownComplete`]
    /// before returning.
    async fn close(&self) -> Result<(), LibraryError>;
}

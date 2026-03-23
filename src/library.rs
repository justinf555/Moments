pub mod bundle;
pub mod config;
pub mod error;
pub mod event;
pub mod factory;
pub mod local;
pub mod storage;

pub use error::LibraryError;
pub use event::LibraryEvent;

use storage::LibraryStorage;

/// The public interface for a Moments library backend.
///
/// `Library` extends [`LibraryStorage`], so every backend must implement both.
/// The GTK application holds a `Box<dyn Library>` and calls methods on it
/// directly. It never imports or references concrete backend types — those are
/// constructed once by [`factory::LibraryFactory`] and erased behind this trait.
///
/// `close()` is inherited from `LibraryStorage` and is the single shutdown
/// point. It is not duplicated here.
///
/// # Implementing this trait
/// - Implement [`LibraryStorage`] first — `open()` receives the [`bundle::Bundle`]
///   and `Sender<LibraryEvent>` for the backend's lifetime.
/// - Never import GTK or adw types.
/// - Instrument all method implementations with `#[instrument]`.
pub trait Library: LibraryStorage {}

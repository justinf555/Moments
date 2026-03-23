pub mod bundle;
pub mod config;
pub mod db;
pub mod error;
pub mod event;
pub mod factory;
pub mod import;
pub mod importer;
pub mod local;
pub mod media;
pub mod storage;

pub use error::LibraryError;
pub use event::LibraryEvent;

use import::LibraryImport;
use storage::LibraryStorage;

/// The public interface for a Moments library backend.
///
/// `Library` is a blanket-impl composition of feature sub-traits. The GTK
/// application holds a `Box<dyn Library>` and calls methods on it directly.
/// It never imports or references concrete backend types.
///
/// New capabilities are added as additional sub-traits per feature issue:
/// - [`LibraryStorage`] — lifecycle (open / close)
/// - [`LibraryImport`]  — photo / video import (issue #5)
///
/// `close()` is inherited from `LibraryStorage` and is not duplicated here.
pub trait Library: LibraryStorage + LibraryImport {}

impl<T: LibraryStorage + LibraryImport> Library for T {}

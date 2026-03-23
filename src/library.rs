pub mod bundle;
pub mod config;
pub mod db;
pub mod error;
pub mod event;
pub mod exif;
pub mod factory;
pub mod import;
pub mod importer;
pub mod media;
pub mod providers;
pub mod storage;
pub mod thumbnail;
pub mod thumbnailer;

use import::LibraryImport;
use media::LibraryMedia;
use storage::LibraryStorage;
use thumbnail::LibraryThumbnail;

/// The public interface for a Moments library backend.
///
/// `Library` is a blanket-impl composition of feature sub-traits. The GTK
/// application holds a `Box<dyn Library>` and calls methods on it directly.
/// It never imports or references concrete backend types.
///
/// New capabilities are added as additional sub-traits per feature issue:
/// - [`LibraryStorage`]   — lifecycle (open / close)
/// - [`LibraryImport`]    — photo / video import (issue #5)
/// - [`LibraryMedia`]     — media asset persistence (issue #25)
/// - [`LibraryThumbnail`] — thumbnail generation and path resolution (issue #6)
///
/// `close()` is inherited from `LibraryStorage` and is not duplicated here.
pub trait Library: LibraryStorage + LibraryImport + LibraryMedia + LibraryThumbnail + Send + Sync {}

impl<T: LibraryStorage + LibraryImport + LibraryMedia + LibraryThumbnail + Send + Sync> Library for T {}

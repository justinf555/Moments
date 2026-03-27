pub mod album;
pub mod bundle;
pub mod config;
pub mod db;
pub mod editing;
pub mod error;
pub mod event;
pub mod exif;
pub mod faces;
pub mod factory;
pub mod format;
pub mod immich_client;
pub mod immich_importer;
pub mod import;
pub mod keyring;
pub mod importer;
pub mod media;
pub mod providers;
pub mod storage;
pub mod sync;
pub mod thumbnail;
pub mod thumbnailer;
pub mod video_meta;
pub mod viewer;

use album::LibraryAlbums;
use editing::LibraryEditing;
use faces::LibraryFaces;
use import::LibraryImport;
use media::LibraryMedia;
use storage::LibraryStorage;
use thumbnail::LibraryThumbnail;
use viewer::LibraryViewer;

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
/// - [`LibraryViewer`]    — detail-view data access (issue #10)
/// - [`LibraryAlbums`]    — album management (issue #11)
/// - [`LibraryFaces`]     — face/people management (issue #178)
/// - [`LibraryEditing`]   — non-destructive photo editing (issue #17)
///
/// `close()` is inherited from `LibraryStorage` and is not duplicated here.
pub trait Library:
    LibraryStorage
    + LibraryImport
    + LibraryMedia
    + LibraryThumbnail
    + LibraryViewer
    + LibraryAlbums
    + LibraryFaces
    + LibraryEditing
    + Send
    + Sync
{
}

impl<
    T: LibraryStorage
        + LibraryImport
        + LibraryMedia
        + LibraryThumbnail
        + LibraryViewer
        + LibraryAlbums
        + LibraryFaces
        + LibraryEditing
        + Send
        + Sync,
> Library for T
{
}

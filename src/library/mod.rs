pub mod album;
pub mod bundle;
pub mod commands;
pub mod config;
pub mod db;
pub mod editing;
pub mod error;
pub mod faces;
pub mod factory;
pub mod format;
pub mod immich_client;
pub mod immich_importer;
pub mod keyring;
pub mod media;
pub mod metadata;
pub mod providers;
pub mod storage;
pub mod sync;
pub mod thumbnail;
pub mod viewer;

use album::LibraryAlbums;
use editing::LibraryEditing;
use faces::LibraryFaces;
use media::LibraryMedia;
use metadata::LibraryMetadata;
use storage::LibraryStorage;
use thumbnail::LibraryThumbnail;
use viewer::LibraryViewer;

/// The public interface for a Moments library backend.
///
/// `Library` is a blanket-impl composition of feature sub-traits. The GTK
/// application holds a `Box<dyn Library>` and calls methods on it directly.
/// It never imports or references concrete backend types.
///
/// Import is handled by the top-level `importer` pipeline (not a library
/// concern). See `src/importer/` and `src/client/import_client.rs`.
///
/// Sub-traits:
/// - [`LibraryStorage`]   — lifecycle (open / close)
/// - [`LibraryMedia`]     — media asset persistence (issue #25)
/// - [`LibraryMetadata`]  — EXIF / media metadata (issue #25)
/// - [`LibraryThumbnail`] — thumbnail path resolution and status (issue #6)
/// - [`LibraryViewer`]    — detail-view data access (issue #10)
/// - [`LibraryAlbums`]    — album management (issue #11)
/// - [`LibraryFaces`]     — face/people management (issue #178)
/// - [`LibraryEditing`]   — non-destructive photo editing (issue #17)
pub trait Library:
    LibraryStorage
    + LibraryMedia
    + LibraryMetadata
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
            + LibraryMedia
            + LibraryMetadata
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

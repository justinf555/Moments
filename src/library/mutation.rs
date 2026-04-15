//! Library mutation types.
//!
//! Every state change the library can produce is represented as a
//! [`Mutation`] variant. Consumers (sync outbox, UI clients) decide
//! what to do with each mutation.

use std::path::PathBuf;

use crate::library::album::AlbumId;
use crate::library::faces::PersonId;
use crate::library::media::MediaId;

/// A state change produced by a library service.
#[derive(Debug, Clone)]
pub enum Mutation {
    // ── Asset ────────────────────────────────────────────────────────

    /// A new asset was imported locally and may need uploading.
    AssetImported { id: MediaId, file_path: PathBuf },

    /// One or more assets had their favourite state changed.
    AssetFavorited { ids: Vec<MediaId>, favorite: bool },

    /// One or more assets were moved to the trash.
    AssetTrashed { ids: Vec<MediaId> },

    /// One or more assets were restored from the trash.
    AssetRestored { ids: Vec<MediaId> },

    /// One or more assets were permanently deleted.
    AssetDeleted { ids: Vec<MediaId> },

    // ── Album ────────────────────────────────────────────────────────

    /// A new album was created.
    AlbumCreated { id: AlbumId, name: String },

    /// An album was renamed.
    AlbumRenamed { id: AlbumId, name: String },

    /// An album was deleted.
    AlbumDeleted { id: AlbumId },

    /// Media items were added to an album.
    AlbumMediaAdded {
        album_id: AlbumId,
        media_ids: Vec<MediaId>,
    },

    /// Media items were removed from an album.
    AlbumMediaRemoved {
        album_id: AlbumId,
        media_ids: Vec<MediaId>,
    },

    // ── People ───────────────────────────────────────────────────────

    /// A person was renamed.
    PersonRenamed { id: PersonId, name: String },

    /// A person's hidden state was changed.
    PersonHidden { id: PersonId, hidden: bool },
}

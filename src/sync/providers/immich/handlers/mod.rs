//! Sync entity handlers.
//!
//! Each handler processes one Immich sync entity type (e.g. `AssetV1`,
//! `AlbumDeleteV1`). The [`PullManager`](super::pull::PullManager)
//! dispatches to the matching handler for each line in the sync stream.

mod album;
mod album_asset;
mod asset;
mod asset_exif;
mod asset_face;
mod person;
mod sync_lifecycle;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::Library;

use super::client::ImmichClient;

pub use album::AlbumDeleteHandler;
pub use album::AlbumHandler;
pub use album_asset::AlbumAssetDeleteHandler;
pub use album_asset::AlbumAssetHandler;
pub use asset::AssetDeleteHandler;
pub use asset::AssetHandler;
pub use asset_exif::AssetExifHandler;
pub use asset_face::AssetFaceDeleteHandler;
pub use asset_face::AssetFaceHandler;
pub use person::PersonDeleteHandler;
pub use person::PersonHandler;
pub use sync_lifecycle::SyncCompleteHandler;
pub use sync_lifecycle::SyncResetHandler;

/// Which counter to increment on success.
#[derive(Debug, Clone, Copy)]
pub enum CounterKind {
    Assets,
    Exifs,
    Deletes,
    Albums,
    People,
    Faces,
    /// Lifecycle entities (reset, complete) — no counter needed.
    None,
}

/// Result of a successful handler invocation.
pub struct HandlerResult {
    /// Entity ID for audit logging — typically the Immich-side UUID
    /// from the sync payload.
    pub entity_id: String,
    /// Local [`MediaId`] for the row this handler touched, if any.
    /// Populated by `AssetHandler` and `AssetDeleteHandler` so that
    /// pull-side orphan tracking during a `SyncResetV1` cycle
    /// compares like-for-like — the orphan set is loaded from
    /// `media.id` (local namespace) and must be checked off against
    /// handler results in that same namespace, not against the Immich
    /// UUID. See issue #628 for the data-loss bug this prevents.
    /// Other handlers (album, exif, face, lifecycle) leave this
    /// `None`; they don't drive orphan tracking.
    ///
    /// Typed as [`MediaId`] rather than `String` so the namespace
    /// invariant is enforced at compile time — it's not possible to
    /// accidentally store an Immich UUID here.
    pub local_media_id: Option<MediaId>,
    /// Audit action label (e.g. "upsert", "delete").
    pub audit_action: &'static str,
    /// Which counter to increment.
    pub counter: CounterKind,
}

/// Shared context available to all handlers.
pub struct SyncContext {
    pub client: ImmichClient,
    pub library: Arc<Library>,
    pub db: Database,
    pub thumbnails_dir: PathBuf,
}

/// A handler for one Immich sync entity type.
#[async_trait]
pub trait SyncEntityHandler: Send + Sync {
    /// The entity type string this handler matches (e.g. "AssetV1").
    fn entity_type(&self) -> &'static str;

    /// Deserialize the raw JSON data, apply the change, and return
    /// a result for audit/counter bookkeeping.
    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError>;
}

/// Build the full set of handlers for the Immich sync stream.
pub fn all_handlers() -> Vec<Box<dyn SyncEntityHandler>> {
    vec![
        Box::new(SyncResetHandler),
        Box::new(SyncCompleteHandler),
        Box::new(AssetHandler),
        Box::new(AssetDeleteHandler),
        Box::new(AssetExifHandler),
        Box::new(AlbumHandler),
        Box::new(AlbumDeleteHandler),
        Box::new(AlbumAssetHandler),
        Box::new(AlbumAssetDeleteHandler),
        Box::new(PersonHandler),
        Box::new(PersonDeleteHandler),
        Box::new(AssetFaceHandler),
        Box::new(AssetFaceDeleteHandler),
    ]
}

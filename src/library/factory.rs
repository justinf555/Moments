use std::sync::mpsc::Sender;

use tracing::instrument;

use super::config::{BackendConfig, LibraryConfig};
use super::error::LibraryError;
use super::event::LibraryEvent;
use super::Library;

/// Creates `Library` instances from a [`LibraryConfig`].
///
/// This is the **only** place in the codebase where concrete backend types are
/// named and constructed. All callers receive a `Box<dyn Library>` and remain
/// entirely unaware of which backend is active.
pub struct LibraryFactory;

impl LibraryFactory {
    /// Construct and open the appropriate backend from `config`.
    ///
    /// The `events` sender is stored inside the backend for its lifetime.
    /// The caller holds the corresponding `Receiver<LibraryEvent>` and polls
    /// it via `glib::idle_add` to receive updates on the GTK main thread.
    #[instrument(skip(_events), fields(bundle_path = %config.bundle_path.display()))]
    pub async fn create(
        config: LibraryConfig,
        _events: Sender<LibraryEvent>,
    ) -> Result<Box<dyn Library>, LibraryError> {
        match config.backend {
            BackendConfig::Local => {
                // Implemented in issue #5 — local backend
                todo!("Local backend not yet implemented")
            }
            BackendConfig::Immich { .. } => {
                // Implemented in issue #14 — Immich backend
                todo!("Immich backend not yet implemented")
            }
        }
    }
}

use std::sync::mpsc::Sender;

use tracing::instrument;

use super::bundle::Bundle;
use super::config::LibraryConfig;
use super::error::LibraryError;
use super::event::LibraryEvent;
use super::local::LocalLibrary;
use super::storage::LibraryStorage;
use super::Library;

/// Creates `Library` instances from a [`Bundle`] and [`LibraryConfig`].
///
/// This is the **only** place in the codebase where concrete backend types are
/// named and constructed. All callers receive a `Box<dyn Library>` and remain
/// entirely unaware of which backend is active.
pub struct LibraryFactory;

impl LibraryFactory {
    /// Construct and open the appropriate backend.
    ///
    /// `bundle` is the validated, open library directory.
    /// `config` identifies the backend and its connection details; it is read
    /// from `library.toml` by [`Bundle::open`] and passed here directly so
    /// the factory does not need to re-parse the manifest.
    /// `events` is stored inside the backend for its lifetime — the caller
    /// holds the corresponding `Receiver<LibraryEvent>` and polls it via
    /// `glib::idle_add` on the GTK main thread.
    #[instrument(skip(events), fields(backend = ?config))]
    pub async fn create(
        bundle: Bundle,
        config: LibraryConfig,
        events: Sender<LibraryEvent>,
    ) -> Result<Box<dyn Library>, LibraryError> {
        match config {
            LibraryConfig::Local => {
                let library = LocalLibrary::open(bundle, events).await?;
                Ok(Box::new(library))
            }
            LibraryConfig::Immich { .. } => {
                // Implemented in issue #14 — Immich backend
                todo!("Immich backend not yet implemented")
            }
        }
    }
}

//! Shared library-loading pipeline for the two entry points that open a
//! bundle and hand it to [`super::startup::start`].
//!
//! Both `MomentsApplication::on_setup_complete` (wizard finished) and
//! `MomentsApplication::open_library` (saved path on launch) run the same
//! sequence: open the bundle, and for Immich configs resolve the session
//! token from the keyring. The two call sites differ only in *presentation*
//! (the error wording is phrased for each context, and a different window
//! parents the dialog) and in *side effects* on the GSettings library path.
//!
//! To keep that pipeline in exactly one place this module owns the input →
//! ready-library logic and returns a plain-data [`LoadOutcome`]. On failure
//! it reports a typed [`LoadFailure`] describing *what* went wrong (plus
//! whether the saved path is stale); the caller in `application/mod.rs` owns
//! all GTK presentation — it phrases the context-appropriate dialog text,
//! shows the dialog parented to the right window, and writes/clears the
//! GSettings library path.
//!
//! Keeping the loader free of widget and wording concerns makes it pure Rust
//! and lets the pipeline be unit-tested without a GTK `Application`.

use std::path::Path;

use tracing::{error, instrument};

use super::keyring::{self, TokenError};
use crate::library::bundle::Bundle;
use crate::library::config::LibraryConfig;

/// Result of running the load pipeline for a single path.
pub(in crate::application) enum LoadOutcome {
    /// Bundle opened and (for Immich) the token resolved. Ready to start.
    Ready {
        bundle: Bundle,
        config: LibraryConfig,
    },
    /// Loading failed. The caller phrases and presents the error dialog and
    /// decides the GSettings path side effect (see [`LoadFailure::clear_path`]).
    Failed(LoadFailure),
}

/// Why the load pipeline could not produce a ready library.
///
/// Carries the raw detail (paths, underlying error strings) so the caller can
/// compose context-appropriate dialog text; the wording itself lives at the
/// call site, not here.
pub(in crate::application) enum LoadFailure {
    /// `Bundle::open` failed — e.g. the directory was deleted while the
    /// GSettings path entry still existed. `details` is the error string.
    BundleOpen { details: String },
    /// No (or empty) Immich session token in the keyring. The user must
    /// sign in again.
    TokenMissing,
    /// libsecret returned an error (D-Bus unavailable, locked collection,
    /// schema mismatch). Typically transient. `details` is the error string.
    KeyringFailed { details: String },
}

impl LoadFailure {
    /// Whether the saved `library-path` should be cleared in response.
    ///
    /// `true` when the path can never load as-is (deleted bundle, missing
    /// token) so the user should be bounced into fresh setup; `false` for
    /// transient keyring/D-Bus failures where a simple relaunch recovers and
    /// the saved path must stay intact.
    pub(in crate::application) fn clear_path(&self) -> bool {
        match self {
            LoadFailure::BundleOpen { .. } | LoadFailure::TokenMissing => true,
            LoadFailure::KeyringFailed { .. } => false,
        }
    }
}

/// Runs the bundle-open + token-resolve pipeline shared by the wizard and
/// the open-on-launch paths.
pub(in crate::application) struct LibraryLoader;

impl LibraryLoader {
    /// Open the bundle at `path` and resolve any Immich session token.
    ///
    /// Returns [`LoadOutcome::Ready`] on success or [`LoadOutcome::Failed`]
    /// with a typed [`LoadFailure`] otherwise. Logs failures here so the
    /// single pipeline owns the diagnostic logging.
    #[instrument(skip(self), fields(path = %path.display()))]
    pub(in crate::application) fn load(&self, path: &Path) -> LoadOutcome {
        let (bundle, config) = match Bundle::open(path) {
            Ok(result) => result,
            Err(e) => {
                error!("failed to open library bundle: {e}");
                return LoadOutcome::Failed(LoadFailure::BundleOpen {
                    details: e.to_string(),
                });
            }
        };

        let config = match config {
            LibraryConfig::Immich { server_url, .. } => {
                match keyring::resolve_immich_token(&server_url) {
                    Ok(access_token) => LibraryConfig::Immich {
                        server_url,
                        access_token,
                    },
                    Err(TokenError::Missing) => {
                        return LoadOutcome::Failed(LoadFailure::TokenMissing);
                    }
                    Err(TokenError::KeyringFailed(e)) => {
                        error!("keyring lookup failed during library load: {e}");
                        return LoadOutcome::Failed(LoadFailure::KeyringFailed { details: e });
                    }
                }
            }
            other => other,
        };

        LoadOutcome::Ready { bundle, config }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_open_failure_clears_path() {
        assert!(LoadFailure::BundleOpen {
            details: "boom".into()
        }
        .clear_path());
    }

    #[test]
    fn token_missing_clears_path() {
        assert!(LoadFailure::TokenMissing.clear_path());
    }

    #[test]
    fn keyring_failed_keeps_path() {
        assert!(!LoadFailure::KeyringFailed {
            details: "dbus down".into()
        }
        .clear_path());
    }

    #[test]
    fn load_missing_bundle_reports_bundle_open() {
        // A path that does not exist cannot open; the pipeline must classify
        // this as a BundleOpen failure (which clears the stale path).
        let loader = LibraryLoader;
        let outcome = loader.load(Path::new("/nonexistent/moments/library/path"));
        match outcome {
            LoadOutcome::Failed(LoadFailure::BundleOpen { details }) => {
                assert!(!details.is_empty());
            }
            _ => panic!("expected BundleOpen failure for a nonexistent path"),
        }
    }
}

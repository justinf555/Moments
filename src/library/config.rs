/// How the local backend stores original photos.
///
/// Written into the `[local]` section of `library.toml` so the correct
/// behaviour is restored on subsequent launches.
#[derive(Debug, Clone)]
pub enum LocalStorageMode {
    /// Moments copies photos into its own storage inside the app sandbox.
    Managed,
    /// Photos stay at their original location. The database stores absolute
    /// (portal) paths to the originals.
    Referenced,
}

/// Identifies which backend to use and provides its connection details.
///
/// Created during onboarding and written into `library.toml`. On subsequent
/// launches, [`super::bundle::Bundle::open`] reads it back from the manifest
/// so [`super::factory::LibraryFactory`] can construct the correct backend.
#[derive(Clone)]
pub enum LibraryConfig {
    /// Local filesystem backend.
    Local { mode: LocalStorageMode },

    /// Immich server backend — originals live on the server; the bundle caches
    /// metadata and thumbnails locally.
    Immich {
        server_url: String,
        /// Session token obtained via `POST /auth/login`. Stored in GNOME
        /// Keyring, never in the bundle manifest.
        access_token: String,
    },
}

impl std::fmt::Debug for LibraryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local { mode } => f.debug_struct("Local").field("mode", mode).finish(),
            Self::Immich { server_url, .. } => f
                .debug_struct("Immich")
                .field("server_url", server_url)
                .field("access_token", &"[REDACTED]")
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_managed_config_variant() {
        let config = LibraryConfig::Local {
            mode: LocalStorageMode::Managed,
        };
        assert!(matches!(
            config,
            LibraryConfig::Local {
                mode: LocalStorageMode::Managed
            }
        ));
    }

    #[test]
    fn local_referenced_config_variant() {
        let config = LibraryConfig::Local {
            mode: LocalStorageMode::Referenced,
        };
        assert!(matches!(
            config,
            LibraryConfig::Local {
                mode: LocalStorageMode::Referenced
            }
        ));
    }

    #[test]
    fn immich_config_stores_fields() {
        let config = LibraryConfig::Immich {
            server_url: "http://immich.local:2283".to_string(),
            access_token: "test-token".to_string(),
        };
        if let LibraryConfig::Immich {
            server_url,
            access_token,
        } = config
        {
            assert_eq!(server_url, "http://immich.local:2283");
            assert_eq!(access_token, "test-token");
        } else {
            panic!("expected Immich variant");
        }
    }
}

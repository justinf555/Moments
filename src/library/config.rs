/// Identifies which backend to use and provides its connection details.
///
/// Created during onboarding and written into `library.toml`. On subsequent
/// launches, [`super::bundle::Bundle::open`] reads it back from the manifest
/// so [`super::factory::LibraryFactory`] can construct the correct backend.
#[derive(Debug, Clone)]
pub enum LibraryConfig {
    /// Local filesystem backend — originals are imported into the bundle itself.
    Local,

    /// Immich server backend — originals live on the server; the bundle caches
    /// metadata and thumbnails locally.
    Immich {
        server_url: String,
        /// Session token obtained via `POST /auth/login`. Stored in GNOME
        /// Keyring, never in the bundle manifest.
        access_token: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_config_variant() {
        let config = LibraryConfig::Local;
        assert!(matches!(config, LibraryConfig::Local));
    }

    #[test]
    fn immich_config_stores_fields() {
        let config = LibraryConfig::Immich {
            server_url: "http://immich.local:2283".to_string(),
            access_token: "test-token".to_string(),
        };
        if let LibraryConfig::Immich { server_url, access_token } = config {
            assert_eq!(server_url, "http://immich.local:2283");
            assert_eq!(access_token, "test-token");
        } else {
            panic!("expected Immich variant");
        }
    }
}

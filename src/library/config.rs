use std::path::PathBuf;

/// Full configuration needed to open or create a library bundle.
///
/// On first run this is built from the onboarding flow and written to
/// `library.toml` inside the bundle. On subsequent launches `LibraryFactory`
/// reads it back from `library.toml` to reconstruct the correct backend.
#[derive(Debug)]
pub struct LibraryConfig {
    /// Path to the `Moments.library` bundle directory.
    pub bundle_path: PathBuf,

    /// Which backend to use and its connection details.
    pub backend: BackendConfig,
}

/// Backend-specific configuration variants.
#[derive(Debug)]
pub enum BackendConfig {
    /// Local filesystem backend — originals are imported into the bundle itself.
    Local,

    /// Immich server backend — originals live on the server; the bundle caches
    /// metadata and thumbnails locally.
    Immich {
        server_url: String,
        api_key: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_config_stores_bundle_path() {
        let config = LibraryConfig {
            bundle_path: PathBuf::from("/home/user/Moments.library"),
            backend: BackendConfig::Local,
        };
        assert_eq!(config.bundle_path, PathBuf::from("/home/user/Moments.library"));
        assert!(matches!(config.backend, BackendConfig::Local));
    }

    #[test]
    fn immich_config_stores_fields() {
        let config = LibraryConfig {
            bundle_path: PathBuf::from("/home/user/Moments.library"),
            backend: BackendConfig::Immich {
                server_url: "http://immich.local:2283".to_string(),
                api_key: "abc123".to_string(),
            },
        };
        assert!(matches!(config.backend, BackendConfig::Immich { .. }));
        if let BackendConfig::Immich { server_url, api_key } = config.backend {
            assert_eq!(server_url, "http://immich.local:2283");
            assert_eq!(api_key, "abc123");
        }
    }
}

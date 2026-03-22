use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument};

use super::config::LibraryConfig;
use super::error::LibraryError;

/// Current bundle format version written to `library.toml`.
const BUNDLE_VERSION: u32 = 1;

/// Filename of the bundle manifest inside the library directory.
const MANIFEST_FILE: &str = "library.toml";

// ── Manifest types ────────────────────────────────────────────────────────────

/// Deserialised representation of `library.toml`.
#[derive(Debug, Serialize, Deserialize)]
pub struct LibraryManifest {
    pub library: LibrarySection,
    pub immich: Option<ImmichSection>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LibrarySection {
    pub version: u32,
    /// `"local"` or `"immich"`
    pub backend: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImmichSection {
    pub server_url: String,
}

impl LibraryManifest {
    /// Build a manifest from a [`LibraryConfig`], ready to be written to `library.toml`.
    fn new(config: &LibraryConfig) -> Self {
        match config {
            LibraryConfig::Local => Self {
                library: LibrarySection {
                    version: BUNDLE_VERSION,
                    backend: "local".to_string(),
                },
                immich: None,
            },
            LibraryConfig::Immich { server_url, .. } => Self {
                library: LibrarySection {
                    version: BUNDLE_VERSION,
                    backend: "immich".to_string(),
                },
                immich: Some(ImmichSection {
                    server_url: server_url.clone(),
                }),
            },
        }
    }
}

impl LibraryConfig {
    /// Parse a [`LibraryConfig`] from a manifest read out of `library.toml`.
    ///
    /// Returns an error if the backend type is unrecognised or required fields
    /// are missing.
    fn from_manifest(manifest: &LibraryManifest) -> Result<Self, LibraryError> {
        match manifest.library.backend.as_str() {
            "local" => Ok(LibraryConfig::Local),
            "immich" => {
                let immich = manifest.immich.as_ref().ok_or_else(|| {
                    LibraryError::Bundle(
                        "[immich] section missing from library.toml".to_string(),
                    )
                })?;
                Ok(LibraryConfig::Immich {
                    server_url: immich.server_url.clone(),
                    // api_key is never stored in library.toml — fetched from
                    // the system keyring by the Immich backend on open()
                    api_key: String::new(),
                })
            }
            other => Err(LibraryError::InvalidBackend(other.to_string())),
        }
    }
}

// ── Bundle ────────────────────────────────────────────────────────────────────

/// A validated, open `Moments.library` bundle directory.
///
/// Holds typed paths to every well-known subdirectory. Subdirectories are
/// created lazily by the feature or backend that first needs them — not
/// upfront at bundle creation time.
///
/// Obtain via [`Bundle::create`] on first run or [`Bundle::open`] on
/// subsequent runs.
#[derive(Debug)]
pub struct Bundle {
    /// Root bundle directory, e.g. `~/Pictures/Moments.library`.
    pub path: PathBuf,
    /// `<bundle>/originals/` — source files (local backend; created on first import).
    pub originals: PathBuf,
    /// `<bundle>/thumbnails/` — generated thumbnails (created when thumbnail generation runs).
    pub thumbnails: PathBuf,
    /// `<bundle>/faces/` — face recognition data (created when face detection runs).
    pub faces: PathBuf,
    /// `<bundle>/database/` — local SQLite store (created when database is initialised).
    pub database: PathBuf,
}

impl Bundle {
    fn manifest_path(bundle_path: &Path) -> PathBuf {
        bundle_path.join(MANIFEST_FILE)
    }

    /// Private constructor — builds path fields from a root path.
    /// Shared by [`Bundle::create`] and [`Bundle::open`] to avoid duplication.
    fn from_path(path: &Path) -> Self {
        Self {
            originals: path.join("originals"),
            thumbnails: path.join("thumbnails"),
            faces: path.join("faces"),
            database: path.join("database"),
            path: path.to_path_buf(),
        }
    }

    /// Create a new library bundle at `path`.
    ///
    /// Creates the root directory and writes `library.toml`. Subdirectories
    /// are **not** created here — each feature creates its own when first needed.
    ///
    /// Returns an error if a file or directory already exists at `path`.
    #[instrument(fields(path = %path.display()))]
    pub fn create(path: &Path, config: &LibraryConfig) -> Result<Self, LibraryError> {
        if path.exists() {
            return Err(LibraryError::Bundle(format!(
                "bundle already exists at {}",
                path.display()
            )));
        }

        info!("creating new library bundle");

        fs::create_dir_all(path)?;

        let manifest = LibraryManifest::new(config);
        let toml_content = toml::to_string(&manifest)
            .map_err(|e| LibraryError::Bundle(format!("failed to serialise manifest: {e}")))?;

        let manifest_path = Self::manifest_path(path);
        debug!(path = %manifest_path.display(), "writing library.toml");
        fs::write(&manifest_path, toml_content)?;

        info!("library bundle created successfully");

        Ok(Self::from_path(path))
    }

    /// Open an existing library bundle at `path`.
    ///
    /// Reads and parses `library.toml`, then returns the bundle alongside the
    /// [`LibraryConfig`] stored in the manifest so that
    /// [`super::factory::LibraryFactory`] can construct the correct backend.
    #[instrument(fields(path = %path.display()))]
    pub fn open(path: &Path) -> Result<(Self, LibraryConfig), LibraryError> {
        if !path.is_dir() {
            return Err(LibraryError::Bundle(format!(
                "bundle not found at {}",
                path.display()
            )));
        }

        info!("opening library bundle");

        let manifest_path = Self::manifest_path(path);
        let toml_content = fs::read_to_string(&manifest_path)
            .map_err(|e| LibraryError::Bundle(format!("failed to read library.toml: {e}")))?;

        let manifest: LibraryManifest = toml::from_str(&toml_content)
            .map_err(|e| LibraryError::Bundle(format!("failed to parse library.toml: {e}")))?;

        debug!(
            version = manifest.library.version,
            backend = %manifest.library.backend,
            "manifest loaded"
        );

        let config = LibraryConfig::from_manifest(&manifest)?;

        info!("library bundle opened successfully");

        Ok((Self::from_path(path), config))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_local_bundle_creates_root_directory() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");

        let bundle = Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();

        assert!(bundle.path.is_dir());
    }

    #[test]
    fn create_local_bundle_does_not_create_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");

        let bundle = Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();

        assert!(!bundle.originals.exists());
        assert!(!bundle.thumbnails.exists());
        assert!(!bundle.faces.exists());
        assert!(!bundle.database.exists());
    }

    #[test]
    fn create_local_bundle_writes_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");

        Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();

        let manifest_path = bundle_path.join(MANIFEST_FILE);
        assert!(manifest_path.exists());

        let content = fs::read_to_string(&manifest_path).unwrap();
        assert!(content.contains("local"));
        assert!(content.contains("version"));
    }

    #[test]
    fn create_over_existing_path_fails() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");

        Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();
        let result = Bundle::create(&bundle_path, &LibraryConfig::Local);

        assert!(matches!(result, Err(LibraryError::Bundle(_))));
    }

    #[test]
    fn open_local_bundle_returns_correct_config() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");

        Bundle::create(&bundle_path, &LibraryConfig::Local).unwrap();
        let (bundle, config) = Bundle::open(&bundle_path).unwrap();

        assert_eq!(bundle.path, bundle_path);
        assert!(matches!(config, LibraryConfig::Local));
    }

    #[test]
    fn open_immich_bundle_returns_server_url() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");

        let config = LibraryConfig::Immich {
            server_url: "http://immich.local:2283".to_string(),
            api_key: "secret".to_string(),
        };
        Bundle::create(&bundle_path, &config).unwrap();
        let (_, restored) = Bundle::open(&bundle_path).unwrap();

        if let LibraryConfig::Immich { server_url, .. } = restored {
            assert_eq!(server_url, "http://immich.local:2283");
        } else {
            panic!("expected Immich config");
        }
    }

    #[test]
    fn open_nonexistent_bundle_fails() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Missing.library");

        let result = Bundle::open(&bundle_path);
        assert!(matches!(result, Err(LibraryError::Bundle(_))));
    }

    #[test]
    fn manifest_roundtrip_local() {
        let manifest = LibraryManifest::new(&LibraryConfig::Local);
        let config = LibraryConfig::from_manifest(&manifest).unwrap();
        assert!(matches!(config, LibraryConfig::Local));
    }

    #[test]
    fn manifest_roundtrip_immich() {
        let config = LibraryConfig::Immich {
            server_url: "http://test:2283".to_string(),
            api_key: "key".to_string(),
        };
        let manifest = LibraryManifest::new(&config);
        let restored = LibraryConfig::from_manifest(&manifest).unwrap();

        if let LibraryConfig::Immich { server_url, .. } = restored {
            assert_eq!(server_url, "http://test:2283");
        } else {
            panic!("expected Immich config");
        }
    }

    #[test]
    fn manifest_unknown_backend_returns_invalid_backend_error() {
        let manifest = LibraryManifest {
            library: LibrarySection {
                version: 1,
                backend: "s3".to_string(),
            },
            immich: None,
        };
        let result = LibraryConfig::from_manifest(&manifest);
        assert!(matches!(result, Err(LibraryError::InvalidBackend(_))));
    }

    #[test]
    fn immich_manifest_missing_section_returns_bundle_error() {
        let manifest = LibraryManifest {
            library: LibrarySection {
                version: 1,
                backend: "immich".to_string(),
            },
            immich: None,
        };
        let result = LibraryConfig::from_manifest(&manifest);
        assert!(matches!(result, Err(LibraryError::Bundle(_))));
    }
}

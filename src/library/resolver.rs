// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Original file resolution trait.
//!
//! Abstracts how the original asset file is located or fetched.
//! Local backends resolve a filesystem path directly; Immich backends
//! check a local cache first and fetch from the server on miss.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::config::LocalStorageMode;
use super::error::LibraryError;
use super::media::MediaId;

/// Resolves the original file for a media asset.
///
/// Injected into [`super::media::MediaService`] at construction time.
#[async_trait]
pub trait OriginalResolver: Send + Sync {
    /// Return the filesystem path to the original file.
    ///
    /// For local backends this is immediate. For remote backends this
    /// may involve downloading the file to a local cache first.
    async fn resolve(
        &self,
        id: &MediaId,
        relative_path: &str,
        original_filename: Option<&str>,
        external_id: Option<&str>,
    ) -> Result<Option<PathBuf>, LibraryError>;
}

/// Registry of originals that are in use and must not be evicted from a
/// resolver's cache (open in the viewer, being exported, mid-download).
///
/// Pins are reference-counted per [`MediaId`]: the same original can be
/// pinned by several holders and stays pinned until every
/// [`OriginalPin`] for it is dropped. Backends without a cache ignore it.
#[derive(Debug, Default)]
pub struct OriginalPins {
    counts: Mutex<HashMap<MediaId, usize>>,
}

impl OriginalPins {
    /// Pin `id` until the returned guard is dropped.
    pub fn pin(self: &Arc<Self>, id: &MediaId) -> OriginalPin {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry(id.clone()).or_insert(0) += 1;
        OriginalPin {
            pins: Arc::clone(self),
            id: id.clone(),
        }
    }

    /// Whether at least one live [`OriginalPin`] exists for `id`.
    pub fn is_pinned(&self, id: &MediaId) -> bool {
        self.counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }

    /// Run `f` only if `id` is not pinned, holding the registry lock so
    /// no pin can be taken while it runs. Returns `None` when pinned.
    ///
    /// `f` must be short (one syscall, e.g. deleting a cached file).
    pub fn run_if_unpinned<R>(&self, id: &MediaId, f: impl FnOnce() -> R) -> Option<R> {
        let counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if counts.contains_key(id) {
            None
        } else {
            Some(f())
        }
    }

    fn unpin(&self, id: &MediaId) {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = counts.get_mut(id) {
            *count -= 1;
            if *count == 0 {
                counts.remove(id);
            }
        }
    }
}

/// RAII guard returned by [`OriginalPins::pin`]; unpins on drop.
#[derive(Debug)]
pub struct OriginalPin {
    pins: Arc<OriginalPins>,
    id: MediaId,
}

impl Drop for OriginalPin {
    fn drop(&mut self) {
        self.pins.unpin(&self.id);
    }
}

/// Local filesystem resolver — returns the path directly, no network.
pub struct LocalResolver {
    originals_dir: PathBuf,
    mode: LocalStorageMode,
}

impl LocalResolver {
    pub fn new(originals_dir: PathBuf, mode: LocalStorageMode) -> Self {
        Self {
            originals_dir,
            mode,
        }
    }
}

#[async_trait]
impl OriginalResolver for LocalResolver {
    async fn resolve(
        &self,
        _id: &MediaId,
        relative_path: &str,
        _original_filename: Option<&str>,
        _external_id: Option<&str>,
    ) -> Result<Option<PathBuf>, LibraryError> {
        let path = match self.mode {
            LocalStorageMode::Referenced => PathBuf::from(relative_path),
            LocalStorageMode::Managed => self.originals_dir.join(relative_path),
        };
        // A failed existence check (e.g. permission denied) is treated as
        // missing, matching the old `Path::exists` behaviour.
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            Ok(Some(path))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_is_reference_counted() {
        let pins = Arc::new(OriginalPins::default());
        let id = MediaId::new("aabb".to_string());
        assert!(!pins.is_pinned(&id));

        let first = pins.pin(&id);
        let second = pins.pin(&id);
        assert!(pins.is_pinned(&id));

        drop(first);
        assert!(pins.is_pinned(&id), "still pinned by the second guard");
        drop(second);
        assert!(!pins.is_pinned(&id));
    }

    #[test]
    fn pins_are_per_id() {
        let pins = Arc::new(OriginalPins::default());
        let a = MediaId::new("aaaa".to_string());
        let b = MediaId::new("bbbb".to_string());
        let _pin = pins.pin(&a);
        assert!(pins.is_pinned(&a));
        assert!(!pins.is_pinned(&b));
    }

    #[test]
    fn run_if_unpinned_skips_pinned_ids() {
        let pins = Arc::new(OriginalPins::default());
        let a = MediaId::new("aaaa".to_string());
        let b = MediaId::new("bbbb".to_string());
        let _pin = pins.pin(&a);
        assert_eq!(pins.run_if_unpinned(&a, || 1), None);
        assert_eq!(pins.run_if_unpinned(&b, || 2), Some(2));
    }

    #[tokio::test]
    async fn managed_mode_joins_originals_dir() {
        let dir = tempfile::tempdir().unwrap();
        let originals = dir.path().join("originals");
        std::fs::create_dir_all(&originals).unwrap();

        // Create a file at originals/2025/01/photo.jpg
        let photo_dir = originals.join("2025").join("01");
        std::fs::create_dir_all(&photo_dir).unwrap();
        let photo_path = photo_dir.join("photo.jpg");
        std::fs::write(&photo_path, b"jpeg data").unwrap();

        let resolver = LocalResolver::new(originals.clone(), super::LocalStorageMode::Managed);
        let id = super::super::media::MediaId::new("test-id".to_string());

        let result = resolver
            .resolve(&id, "2025/01/photo.jpg", None, None)
            .await
            .unwrap();
        assert_eq!(result, Some(photo_path));
    }

    #[tokio::test]
    async fn managed_mode_returns_none_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        let resolver =
            LocalResolver::new(dir.path().to_path_buf(), super::LocalStorageMode::Managed);
        let id = super::super::media::MediaId::new("test-id".to_string());

        let result = resolver
            .resolve(&id, "nonexistent/photo.jpg", None, None)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn referenced_mode_uses_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let photo_path = dir.path().join("my_photo.jpg");
        std::fs::write(&photo_path, b"jpeg data").unwrap();

        let resolver = LocalResolver::new(
            dir.path().to_path_buf(),
            super::LocalStorageMode::Referenced,
        );
        let id = super::super::media::MediaId::new("test-id".to_string());

        // In referenced mode, relative_path is actually the absolute path.
        let result = resolver
            .resolve(&id, photo_path.to_str().unwrap(), None, None)
            .await
            .unwrap();
        assert_eq!(result, Some(photo_path));
    }

    #[tokio::test]
    async fn referenced_mode_returns_none_for_missing() {
        let resolver = LocalResolver::new(
            std::path::PathBuf::from("/unused"),
            super::LocalStorageMode::Referenced,
        );
        let id = super::super::media::MediaId::new("test-id".to_string());

        let result = resolver
            .resolve(&id, "/absolutely/nonexistent/photo.jpg", None, None)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn managed_mode_ignores_media_id() {
        // The LocalResolver does not use the MediaId for path resolution.
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.jpg");
        std::fs::write(&photo, b"data").unwrap();

        let resolver =
            LocalResolver::new(dir.path().to_path_buf(), super::LocalStorageMode::Managed);

        let id1 = super::super::media::MediaId::new("id-a".to_string());
        let id2 = super::super::media::MediaId::new("id-b".to_string());

        let r1 = resolver
            .resolve(&id1, "photo.jpg", None, None)
            .await
            .unwrap();
        let r2 = resolver
            .resolve(&id2, "photo.jpg", None, None)
            .await
            .unwrap();
        assert_eq!(r1, r2);
    }
}

// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;

use tokio::sync::mpsc;
use tracing::{debug, instrument};

use super::event::ThumbnailEvent;
use super::model::ThumbnailStatus;
use super::repository::ThumbnailRepository;
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::event_emitter::EventEmitter;
use crate::library::media::MediaId;

/// Compute the two-level sharded thumbnail path.
///
/// Extracted as a free function so both the service and internal
/// components (thumbnailer, sync downloader) can use the same logic.
pub fn sharded_thumbnail_path(thumbnails_dir: &std::path::Path, id: &MediaId) -> PathBuf {
    let hex = id.as_str();
    if hex.len() < 4 {
        return thumbnails_dir.join(format!("{hex}.webp"));
    }
    thumbnails_dir
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(format!("{hex}.webp"))
}

/// Thumbnail path resolution and persistence service.
///
/// Holds an [`EventEmitter<ThumbnailEvent>`] to notify clients when a
/// thumbnail becomes ready on disk. Each call to [`subscribe`] returns a
/// fresh receiver; every emitted event is delivered to every live
/// subscriber.
///
/// [`subscribe`]: ThumbnailService::subscribe
#[derive(Clone)]
pub struct ThumbnailService {
    repo: ThumbnailRepository,
    thumbnails_dir: PathBuf,
    events: EventEmitter<ThumbnailEvent>,
}

impl ThumbnailService {
    pub fn new(db: Database, thumbnails_dir: PathBuf) -> Self {
        Self {
            repo: ThumbnailRepository::new(db),
            thumbnails_dir,
            events: EventEmitter::new(),
        }
    }

    /// Register a new subscriber. Every emitted event is delivered to every
    /// live subscriber.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<ThumbnailEvent> {
        self.events.subscribe()
    }

    /// Broadcast an event to every live subscriber.
    fn emit(&self, event: ThumbnailEvent) {
        self.events.emit(event);
    }

    pub fn thumbnail_path(&self, id: &MediaId) -> PathBuf {
        sharded_thumbnail_path(&self.thumbnails_dir, id)
    }

    pub async fn insert_thumbnail_pending(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.insert_pending(id).await
    }

    pub async fn set_thumbnail_ready(
        &self,
        id: &MediaId,
        file_path: &str,
        generated_at: i64,
    ) -> Result<(), LibraryError> {
        self.repo.set_ready(id, file_path, generated_at).await?;
        self.emit(ThumbnailEvent::Ready(id.clone()));
        Ok(())
    }

    pub async fn set_thumbnail_failed(&self, id: &MediaId) -> Result<(), LibraryError> {
        self.repo.set_failed(id).await
    }

    pub async fn thumbnail_status(
        &self,
        id: &MediaId,
    ) -> Result<Option<ThumbnailStatus>, LibraryError> {
        self.repo.status(id).await
    }

    /// Drop the cached thumbnail for `id`: delete the file and the status
    /// row. Idempotent; a thumbnail that was never cached is a no-op.
    ///
    /// Issue #695: an asset that becomes hidden or locked is no longer
    /// listed, and the locked folder is a privacy feature, so its
    /// thumbnail must not stay on disk. If it later returns to the
    /// timeline, the sync handler finds no cached file and downloads it
    /// again.
    #[instrument(skip(self))]
    pub async fn remove_thumbnail(&self, id: &MediaId) -> Result<(), LibraryError> {
        let path = self.thumbnail_path(id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => debug!("removed cached thumbnail"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(LibraryError::Io(e)),
        }
        self.repo.delete(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Issue #695: `remove_thumbnail` deletes the cached file and the
    /// status row, and is a no-op when nothing is cached.
    #[tokio::test]
    async fn remove_thumbnail_deletes_file_and_row() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new();
        db.open(&dir.path().join("test.db")).await.unwrap();
        let svc = ThumbnailService::new(db.clone(), dir.path().join("thumbnails"));

        let id = MediaId::new("ab".repeat(16));
        let media = crate::library::media::repository::MediaRepository::new(db);
        media
            .insert(&crate::library::db::test_helpers::record_with_taken_at(
                id.clone(),
                "a/photo.jpg",
                None,
            ))
            .await
            .unwrap();

        // Never cached: no-op.
        svc.remove_thumbnail(&id).await.unwrap();

        let path = svc.thumbnail_path(&id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"webp").unwrap();
        svc.insert_thumbnail_pending(&id).await.unwrap();
        svc.set_thumbnail_ready(&id, &path.to_string_lossy(), 1)
            .await
            .unwrap();

        svc.remove_thumbnail(&id).await.unwrap();
        assert!(!path.exists());
        assert_eq!(svc.thumbnail_status(&id).await.unwrap(), None);
    }

    #[test]
    fn sharded_path_normal_id() {
        let id = MediaId::new("abcdef1234567890".to_string());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/ab/cd/abcdef1234567890.webp"));
    }

    #[test]
    fn sharded_path_short_id_no_panic() {
        let id = MediaId::new("ab".to_string());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/ab.webp"));
    }

    #[test]
    fn sharded_path_empty_id_no_panic() {
        let id = MediaId::new(String::new());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/.webp"));
    }

    #[test]
    fn sharded_path_exactly_four_chars() {
        let id = MediaId::new("abcd".to_string());
        let path = sharded_thumbnail_path(Path::new("/thumbs"), &id);
        assert_eq!(path, Path::new("/thumbs/ab/cd/abcd.webp"));
    }
}

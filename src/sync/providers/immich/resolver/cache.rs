// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Size-bounded LRU eviction for the Immich originals cache (#711).
//!
//! ## LRU source of truth: file mtime
//!
//! Recency lives on disk as each file's modification time. A download
//! sets it, and [`super::CachedResolver`] sets it to "now" on every cache
//! hit ([`touch`]). atime is not used: the filesystem is often mounted
//! `noatime` / `relatime`, so it can't be trusted. An in-memory LRU index
//! was rejected: it would be empty after every restart and drift as the
//! importer and the delete path add and remove files in the same
//! directory behind its back. A directory scan per pass is cheap next to
//! the multi-megabyte download that triggers it, and it runs on a
//! blocking thread.
//!
//! ## What may be evicted
//!
//! In an Immich library the importer copies local originals into the
//! same sharded `originals/` tree, and the file names (fresh `MediaId`s,
//! see #626) don't tell a download apart from a local import. Every file
//! counts towards the limit, but a file is only deleted when
//!
//! - it isn't pinned ([`OriginalPins`]: open in the viewer, mid-download),
//! - and the [`EvictionGuard`] agrees: in the app, the asset has an
//!   Immich `external_id` (the server holds the bytes and the resolver
//!   can fetch them again) and no unfinished outbox entry still needs
//!   the local file (pending upload).
//!
//! Until a guard is installed nothing is deleted, so the cache can never
//! evict before the library is open.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use tracing::{debug, info, instrument, warn};

use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::resolver::OriginalPins;

/// Suffix of in-flight downloads; never counted or evicted as entries.
pub(super) const PART_SUFFIX: &str = ".part";

/// Leftover partial downloads older than this are deleted by a scan.
const STALE_PART_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Convert the `originals-cache-max-mb` setting to a byte limit.
///
/// The schema documents 0 as "disable eviction", so it maps to no limit.
pub fn mb_to_bytes(mb: u32) -> u64 {
    match mb {
        0 => u64::MAX,
        mb => u64::from(mb) * 1024 * 1024,
    }
}

/// Decides whether a cached original may be deleted. Pins are checked
/// separately by the cache; this answers "is the file safe to lose?".
#[async_trait]
pub trait EvictionGuard: Send + Sync {
    async fn may_evict(&self, id: &MediaId) -> bool;
}

/// One file in the cache directory.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub id: MediaId,
    pub path: PathBuf,
    pub size: u64,
    pub last_used: SystemTime,
}

/// What an eviction pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EvictionOutcome {
    pub evicted: usize,
    pub freed_bytes: u64,
    /// Total size of the cache directory after the pass.
    pub remaining_bytes: u64,
}

/// The on-disk originals cache and its eviction policy.
pub struct OriginalsCache {
    dir: PathBuf,
    limit_bytes: Arc<AtomicU64>,
    pins: Arc<OriginalPins>,
    guard: OnceLock<Arc<dyn EvictionGuard>>,
    /// Serialises passes so concurrent downloads don't race each other
    /// over the same victims.
    pass: tokio::sync::Mutex<()>,
}

impl OriginalsCache {
    pub fn new(dir: PathBuf, limit_bytes: Arc<AtomicU64>, pins: Arc<OriginalPins>) -> Self {
        Self {
            dir,
            limit_bytes,
            pins,
            guard: OnceLock::new(),
            pass: tokio::sync::Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn pins(&self) -> &Arc<OriginalPins> {
        &self.pins
    }

    /// Install the guard that enables eviction. Set once, after the
    /// library is open; later calls are ignored.
    pub fn set_guard(&self, guard: Arc<dyn EvictionGuard>) {
        if self.guard.set(guard).is_err() {
            warn!("originals cache eviction guard already set; ignoring");
        }
    }

    /// Current size of everything in the cache directory, in bytes.
    ///
    /// Scans the directory on a blocking thread. Intended for the
    /// preferences "Cache used" row (#712) as well as eviction.
    pub async fn usage_bytes(&self) -> Result<u64, LibraryError> {
        Ok(self.scan().await?.iter().map(|e| e.size).sum())
    }

    /// Evict least-recently-used files until the cache fits the limit.
    ///
    /// Pinned files and files the guard refuses are skipped, so the
    /// cache can stay over the limit when everything left is in use or
    /// not yet uploaded. A byte limit of 0 keeps only those files (the
    /// setting's 0 means "no limit" and never gets here, see
    /// [`mb_to_bytes`]).
    #[instrument(skip(self))]
    pub async fn enforce_limit(&self) -> Result<EvictionOutcome, LibraryError> {
        let _pass = self.pass.lock().await;

        let mut entries = self.scan().await?;
        let mut total: u64 = entries.iter().map(|e| e.size).sum();
        let limit = self.limit_bytes.load(Ordering::Relaxed);
        let mut outcome = EvictionOutcome {
            remaining_bytes: total,
            ..Default::default()
        };
        if total <= limit {
            debug!(total, limit, "originals cache within limit");
            return Ok(outcome);
        }
        let Some(guard) = self.guard.get().cloned() else {
            debug!(
                total,
                limit, "originals cache over limit, eviction not enabled yet"
            );
            return Ok(outcome);
        };

        // Oldest first; the id breaks ties so the order is deterministic.
        entries.sort_by(|a, b| {
            a.last_used
                .cmp(&b.last_used)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });

        for entry in entries {
            if total <= limit {
                break;
            }
            if self.pins.is_pinned(&entry.id) || !guard.may_evict(&entry.id).await {
                continue;
            }
            // Re-check the pin and delete under the pin lock: a viewer
            // that pins after this point finds the file gone and the
            // resolver downloads it again.
            let removed = self
                .pins
                .run_if_unpinned(&entry.id, || std::fs::remove_file(&entry.path));
            match removed {
                None => continue,
                Some(Ok(())) => {
                    debug!(id = %entry.id, size = entry.size, "evicted cached original");
                    outcome.evicted += 1;
                    outcome.freed_bytes += entry.size;
                    total -= entry.size;
                }
                Some(Err(e)) if e.kind() == io::ErrorKind::NotFound => total -= entry.size,
                Some(Err(e)) => {
                    warn!(id = %entry.id, path = %entry.path.display(), "could not evict cached original: {e}");
                }
            }
        }

        outcome.remaining_bytes = total;
        if outcome.evicted > 0 {
            info!(
                evicted = outcome.evicted,
                freed_bytes = outcome.freed_bytes,
                remaining_bytes = total,
                limit,
                "originals cache eviction pass"
            );
        }
        if total > limit {
            debug!(
                total,
                limit, "originals cache still over limit; the rest is in use or not uploaded"
            );
        }
        Ok(outcome)
    }

    async fn scan(&self) -> Result<Vec<CacheEntry>, LibraryError> {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || scan_dir(&dir))
            .await
            .map_err(|e| LibraryError::Runtime(format!("cache scan task: {e}")))?
            .map_err(LibraryError::Io)
    }
}

/// Mark a cached file as just used by setting its mtime to now.
pub(super) async fn touch(path: PathBuf) {
    let result = tokio::task::spawn_blocking(move || {
        std::fs::File::open(&path).and_then(|f| f.set_modified(SystemTime::now()))
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => debug!("could not update cached original mtime: {e}"),
        Err(e) => debug!("cache touch task failed: {e}"),
    }
}

/// Walk the sharded cache directory (blocking). A missing directory is
/// an empty cache. Partial downloads are skipped, and removed once stale.
fn scan_dir(dir: &Path) -> io::Result<Vec<CacheEntry>> {
    let mut entries = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let now = SystemTime::now();

    while let Some(current) = stack.pop() {
        let read = match std::fs::read_dir(&current) {
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for item in read {
            let item = item?;
            let file_type = item.file_type()?;
            let path = item.path();
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            // The file may vanish between readdir and stat (delete path,
            // a concurrent pass); just skip it.
            let Ok(meta) = item.metadata() else { continue };
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

            if name.ends_with(PART_SUFFIX) {
                let age = now.duration_since(modified).unwrap_or_default();
                if age > STALE_PART_AGE {
                    debug!(path = %path.display(), "removing stale partial download");
                    let _ = std::fs::remove_file(&path);
                }
                continue;
            }

            entries.push(CacheEntry {
                id: MediaId::new(name),
                path,
                size: meta.len(),
                last_used: modified,
            });
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    use crate::library::media::sharded_original_path;

    /// Guard that allows everything except a deny-list.
    #[derive(Default)]
    struct FakeGuard {
        deny: Mutex<HashSet<String>>,
    }

    impl FakeGuard {
        fn denying(ids: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                deny: Mutex::new(ids.iter().map(|s| s.to_string()).collect()),
            })
        }
    }

    #[async_trait]
    impl EvictionGuard for FakeGuard {
        async fn may_evict(&self, id: &MediaId) -> bool {
            !self.deny.lock().unwrap().contains(id.as_str())
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        cache: OriginalsCache,
        limit: Arc<AtomicU64>,
        pins: Arc<OriginalPins>,
    }

    fn fixture(limit: u64, guard: Option<Arc<dyn EvictionGuard>>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let limit = Arc::new(AtomicU64::new(limit));
        let pins = Arc::new(OriginalPins::default());
        let cache = OriginalsCache::new(
            dir.path().to_path_buf(),
            Arc::clone(&limit),
            Arc::clone(&pins),
        );
        if let Some(guard) = guard {
            cache.set_guard(guard);
        }
        Fixture {
            _dir: dir,
            cache,
            limit,
            pins,
        }
    }

    /// Write a cached original of `size` bytes last used `age_secs` ago.
    fn put(fx: &Fixture, id: &str, size: usize, age_secs: u64) -> PathBuf {
        let path = sharded_original_path(fx.cache.dir(), &MediaId::new(id.to_string()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![0u8; size]).unwrap();
        let when = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
        path
    }

    fn present(fx: &Fixture, id: &str) -> bool {
        sharded_original_path(fx.cache.dir(), &MediaId::new(id.to_string())).exists()
    }

    fn allow_all() -> Option<Arc<dyn EvictionGuard>> {
        Some(Arc::new(FakeGuard::default()))
    }

    #[test]
    fn mb_to_bytes_converts_mebibytes() {
        assert_eq!(mb_to_bytes(1), 1024 * 1024);
        assert_eq!(mb_to_bytes(2048), 2048 * 1024 * 1024);
        assert_eq!(mb_to_bytes(u32::MAX), u64::from(u32::MAX) * 1024 * 1024);
    }

    #[tokio::test]
    async fn zero_mb_setting_disables_eviction() {
        let fx = fixture(mb_to_bytes(0), allow_all());
        put(&fx, "aaaa0001", 100, 300);
        assert_eq!(fx.cache.enforce_limit().await.unwrap().evicted, 0);
        assert!(present(&fx, "aaaa0001"));
    }

    #[tokio::test]
    async fn within_limit_evicts_nothing() {
        let fx = fixture(1000, allow_all());
        put(&fx, "aaaa0001", 300, 30);
        put(&fx, "aaaa0002", 300, 20);

        let outcome = fx.cache.enforce_limit().await.unwrap();
        assert_eq!(outcome.evicted, 0);
        assert_eq!(outcome.remaining_bytes, 600);
        assert!(present(&fx, "aaaa0001") && present(&fx, "aaaa0002"));
    }

    #[tokio::test]
    async fn evicts_least_recently_used_first() {
        let fx = fixture(250, allow_all());
        put(&fx, "aaaa0001", 100, 10); // newest
        put(&fx, "bbbb0002", 100, 300); // oldest
        put(&fx, "cccc0003", 100, 200);

        let outcome = fx.cache.enforce_limit().await.unwrap();
        assert_eq!(outcome.evicted, 1);
        assert_eq!(outcome.freed_bytes, 100);
        assert_eq!(outcome.remaining_bytes, 200);
        assert!(!present(&fx, "bbbb0002"), "oldest evicted");
        assert!(present(&fx, "cccc0003"));
        assert!(present(&fx, "aaaa0001"));
    }

    #[tokio::test]
    async fn touch_moves_file_to_most_recently_used() {
        let fx = fixture(150, allow_all());
        let old = put(&fx, "aaaa0001", 100, 300);
        put(&fx, "bbbb0002", 100, 100);

        touch(old).await;

        fx.cache.enforce_limit().await.unwrap();
        assert!(present(&fx, "aaaa0001"), "touched file is now newest");
        assert!(!present(&fx, "bbbb0002"));
    }

    #[tokio::test]
    async fn pinned_files_are_skipped() {
        let fx = fixture(100, allow_all());
        put(&fx, "aaaa0001", 100, 300); // oldest, but pinned
        put(&fx, "bbbb0002", 100, 200);
        put(&fx, "cccc0003", 100, 100);

        let pin = fx.pins.pin(&MediaId::new("aaaa0001".to_string()));
        let outcome = fx.cache.enforce_limit().await.unwrap();
        assert_eq!(outcome.evicted, 2);
        assert!(present(&fx, "aaaa0001"));
        assert!(!present(&fx, "bbbb0002"));
        assert!(!present(&fx, "cccc0003"));

        // Once unpinned it's fair game again.
        drop(pin);
        fx.limit.store(0, Ordering::Relaxed);
        fx.cache.enforce_limit().await.unwrap();
        assert!(!present(&fx, "aaaa0001"));
    }

    #[tokio::test]
    async fn guard_refusal_is_respected() {
        // e.g. a local import still waiting to upload.
        let fx = fixture(0, Some(FakeGuard::denying(&["aaaa0001"])));
        put(&fx, "aaaa0001", 100, 300);
        put(&fx, "bbbb0002", 100, 100);

        let outcome = fx.cache.enforce_limit().await.unwrap();
        assert_eq!(outcome.evicted, 1);
        assert_eq!(outcome.remaining_bytes, 100);
        assert!(present(&fx, "aaaa0001"));
        assert!(!present(&fx, "bbbb0002"));
    }

    #[tokio::test]
    async fn no_guard_means_no_eviction() {
        let fx = fixture(0, None);
        put(&fx, "aaaa0001", 100, 300);

        let outcome = fx.cache.enforce_limit().await.unwrap();
        assert_eq!(outcome.evicted, 0);
        assert_eq!(outcome.remaining_bytes, 100);
        assert!(present(&fx, "aaaa0001"));
    }

    #[tokio::test]
    async fn lowering_limit_at_runtime_takes_effect_on_next_pass() {
        let fx = fixture(1000, allow_all());
        put(&fx, "aaaa0001", 200, 300);
        put(&fx, "bbbb0002", 200, 200);
        put(&fx, "cccc0003", 200, 100);

        assert_eq!(fx.cache.enforce_limit().await.unwrap().evicted, 0);

        fx.limit.store(250, Ordering::Relaxed);
        let outcome = fx.cache.enforce_limit().await.unwrap();
        assert_eq!(outcome.evicted, 2);
        assert_eq!(outcome.remaining_bytes, 200);
        assert!(present(&fx, "cccc0003"), "most recent survives");
    }

    #[tokio::test]
    async fn zero_byte_limit_evicts_everything_not_in_use() {
        let fx = fixture(0, allow_all());
        put(&fx, "aaaa0001", 100, 300);
        put(&fx, "bbbb0002", 100, 200);
        let _pin = fx.pins.pin(&MediaId::new("bbbb0002".to_string()));

        let outcome = fx.cache.enforce_limit().await.unwrap();
        assert_eq!(outcome.evicted, 1);
        assert_eq!(outcome.remaining_bytes, 100);
        assert!(!present(&fx, "aaaa0001"));
        assert!(present(&fx, "bbbb0002"));
    }

    #[tokio::test]
    async fn missing_cache_dir_is_empty() {
        let fx = fixture(0, allow_all());
        let gone = OriginalsCache::new(
            fx.cache.dir().join("does-not-exist"),
            Arc::new(AtomicU64::new(0)),
            Arc::new(OriginalPins::default()),
        );
        assert_eq!(gone.usage_bytes().await.unwrap(), 0);
        assert_eq!(
            gone.enforce_limit().await.unwrap(),
            EvictionOutcome::default()
        );
    }

    #[tokio::test]
    async fn partial_downloads_are_not_entries() {
        let fx = fixture(0, allow_all());
        put(&fx, "aaaa0001", 100, 10);
        let part = fx.cache.dir().join("aa").join("aa").join("aaaa0002.1.part");
        std::fs::write(&part, vec![0u8; 500]).unwrap();

        assert_eq!(fx.cache.usage_bytes().await.unwrap(), 100);
        fx.cache.enforce_limit().await.unwrap();
        assert!(part.exists(), "fresh partial download left alone");
    }

    #[tokio::test]
    async fn usage_bytes_sums_all_shards() {
        let fx = fixture(u64::MAX, allow_all());
        put(&fx, "aaaa0001", 100, 10);
        put(&fx, "bbbb0002", 250, 10);
        put(&fx, "ab", 7, 10); // short id, stored flat
        assert_eq!(fx.cache.usage_bytes().await.unwrap(), 357);
    }
}

use std::cell::RefCell;
use std::num::NonZeroUsize;

use gtk::glib;
use lru::LruCache;

use crate::library::media::MediaId;

/// Maximum number of decoded thumbnails to keep in the LRU cache.
/// At ~400KB per thumbnail (320×320 RGBA), 500 entries ~ 195MB RAM.
const DEFAULT_CAPACITY: usize = 500;

/// Decoded RGBA pixel data stored as reference-counted `glib::Bytes`.
///
/// Using `glib::Bytes` instead of `Vec<u8>` allows zero-copy sharing
/// between the cache and `GdkMemoryTexture` — cloning a `glib::Bytes`
/// is just an atomic refcount increment, not a data copy.
struct CachedTexture {
    pixels: glib::Bytes,
    width: u32,
    height: u32,
}

/// LRU cache for decoded thumbnail textures.
///
/// Stores decoded RGBA bytes keyed by [`MediaId`] so that scrolling back
/// to previously-visible cells skips the expensive disk read + image decode.
/// The `GdkTexture` (VRAM) is still cleared on unbind; this cache holds
/// the CPU-side pixel data for fast re-creation.
///
/// Only accessed from the GTK main thread — uses [`RefCell`], not `Mutex`.
pub struct TextureCache {
    inner: RefCell<LruCache<MediaId, CachedTexture>>,
}

impl TextureCache {
    /// Create a new texture cache with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(NonZeroUsize::new(DEFAULT_CAPACITY).expect("DEFAULT_CAPACITY > 0"))
    }

    fn with_capacity(capacity: NonZeroUsize) -> Self {
        Self {
            inner: RefCell::new(LruCache::new(capacity)),
        }
    }

    /// Look up decoded pixel data by media ID.
    ///
    /// Returns a shared `glib::Bytes` on hit and promotes the entry to MRU.
    /// Cloning `glib::Bytes` is a refcount bump — zero data copy.
    pub fn get(&self, id: &MediaId) -> Option<(glib::Bytes, u32, u32)> {
        let mut inner = self.inner.borrow_mut();
        let entry = inner.get(id)?;
        Some((entry.pixels.clone(), entry.width, entry.height))
    }

    /// Insert decoded pixel data into the cache.
    ///
    /// Takes ownership of the `Vec<u8>` and converts it to `glib::Bytes`
    /// once. Returns a clone of the stored `glib::Bytes` (refcount bump)
    /// so the caller can use it directly without a second lookup.
    /// If at capacity, evicts the least-recently-used entry first.
    /// If the key already exists, updates it and promotes to MRU.
    pub fn insert(&self, id: MediaId, pixels: Vec<u8>, width: u32, height: u32) -> glib::Bytes {
        let bytes = glib::Bytes::from_owned(pixels);
        let ret = bytes.clone();
        self.inner.borrow_mut().put(
            id,
            CachedTexture {
                pixels: bytes,
                width,
                height,
            },
        );
        ret
    }
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(s: &str) -> MediaId {
        MediaId::new(s.to_string())
    }

    fn make_pixels(val: u8) -> Vec<u8> {
        vec![val; 100]
    }

    fn cache_with_capacity(capacity: usize) -> TextureCache {
        TextureCache::with_capacity(NonZeroUsize::new(capacity).unwrap())
    }

    #[test]
    fn insert_and_retrieve() {
        let cache = TextureCache::new();
        cache.insert(make_id("a"), make_pixels(1), 10, 10);
        let (pixels, w, h) = cache.get(&make_id("a")).unwrap();
        assert_eq!(&*pixels, &make_pixels(1)[..]);
        assert_eq!(w, 10);
        assert_eq!(h, 10);
    }

    #[test]
    fn miss_returns_none() {
        let cache = TextureCache::new();
        assert!(cache.get(&make_id("missing")).is_none());
    }

    #[test]
    fn evicts_lru_at_capacity() {
        let cache = cache_with_capacity(2);
        cache.insert(make_id("a"), make_pixels(1), 1, 1);
        cache.insert(make_id("b"), make_pixels(2), 1, 1);
        cache.insert(make_id("c"), make_pixels(3), 1, 1);

        assert!(cache.get(&make_id("a")).is_none(), "a should be evicted");
        assert!(cache.get(&make_id("b")).is_some());
        assert!(cache.get(&make_id("c")).is_some());
    }

    #[test]
    fn access_promotes_to_mru() {
        let cache = cache_with_capacity(2);
        cache.insert(make_id("a"), make_pixels(1), 1, 1);
        cache.insert(make_id("b"), make_pixels(2), 1, 1);

        // Access "a" to promote it.
        cache.get(&make_id("a"));

        // Insert "c" — should evict "b" (now LRU), not "a".
        cache.insert(make_id("c"), make_pixels(3), 1, 1);

        assert!(cache.get(&make_id("a")).is_some(), "a should survive");
        assert!(cache.get(&make_id("b")).is_none(), "b should be evicted");
        assert!(cache.get(&make_id("c")).is_some());
    }

    #[test]
    fn duplicate_insert_updates_entry() {
        let cache = TextureCache::new();
        cache.insert(make_id("a"), make_pixels(1), 10, 10);
        cache.insert(make_id("a"), make_pixels(2), 20, 20);

        let (pixels, w, h) = cache.get(&make_id("a")).unwrap();
        assert_eq!(&*pixels, &make_pixels(2)[..]);
        assert_eq!(w, 20);
        assert_eq!(h, 20);
    }
}

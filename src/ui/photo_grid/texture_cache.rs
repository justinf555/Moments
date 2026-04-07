use std::cell::RefCell;
use std::collections::HashMap;

use gtk::glib;

use crate::library::media::MediaId;

/// Maximum number of decoded thumbnails to keep in the LRU cache.
/// At ~355KB per thumbnail (320px RGBA), 500 entries ~ 177MB RAM.
const DEFAULT_CAPACITY: usize = 500;

/// Decoded RGBA pixel data stored as reference-counted `glib::Bytes`.
///
/// Using `glib::Bytes` instead of `Vec<u8>` allows zero-copy sharing
/// between the cache and `GdkMemoryTexture` — cloning a `glib::Bytes`
/// is just an atomic refcount increment, not a data copy.
pub struct CachedTexture {
    pub pixels: glib::Bytes,
    pub width: u32,
    pub height: u32,
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
    inner: RefCell<CacheInner>,
}

struct CacheInner {
    /// Decoded textures keyed by media ID.
    map: HashMap<MediaId, CachedTexture>,
    /// Access order — most recently used at the back.
    order: Vec<MediaId>,
    /// Maximum number of entries before LRU eviction.
    capacity: usize,
}

impl TextureCache {
    /// Create a new texture cache with the default capacity.
    pub fn new() -> Self {
        Self {
            inner: RefCell::new(CacheInner {
                map: HashMap::with_capacity(DEFAULT_CAPACITY),
                order: Vec::with_capacity(DEFAULT_CAPACITY),
                capacity: DEFAULT_CAPACITY,
            }),
        }
    }

    /// Look up decoded pixel data by media ID.
    ///
    /// Returns a shared `glib::Bytes` on hit and promotes the entry to MRU.
    /// Cloning `glib::Bytes` is a refcount bump — zero data copy.
    pub fn get(&self, id: &MediaId) -> Option<(glib::Bytes, u32, u32)> {
        let mut inner = self.inner.borrow_mut();
        if inner.map.contains_key(id) {
            // Promote to MRU.
            if let Some(pos) = inner.order.iter().position(|k| k == id) {
                inner.order.remove(pos);
            }
            inner.order.push(id.clone());
            let entry = &inner.map[id];
            Some((entry.pixels.clone(), entry.width, entry.height))
        } else {
            None
        }
    }

    /// Insert decoded pixel data into the cache.
    ///
    /// Takes ownership of the `Vec<u8>` and converts it to `glib::Bytes`
    /// once. If at capacity, evicts the least-recently-used entry first.
    /// If the key already exists, updates it and promotes to MRU.
    pub fn insert(&self, id: MediaId, pixels: Vec<u8>, width: u32, height: u32) {
        let bytes = glib::Bytes::from_owned(pixels);
        let mut inner = self.inner.borrow_mut();

        // Update existing entry.
        if inner.map.contains_key(&id) {
            inner.map.insert(
                id.clone(),
                CachedTexture {
                    pixels: bytes,
                    width,
                    height,
                },
            );
            if let Some(pos) = inner.order.iter().position(|k| k == &id) {
                inner.order.remove(pos);
            }
            inner.order.push(id);
            return;
        }

        // Evict LRU if at capacity.
        if inner.map.len() >= inner.capacity {
            if let Some(evicted) = inner.order.first().cloned() {
                inner.order.remove(0);
                inner.map.remove(&evicted);
            }
        }

        inner.map.insert(
            id.clone(),
            CachedTexture {
                pixels: bytes,
                width,
                height,
            },
        );
        inner.order.push(id);
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
        let cache = TextureCache {
            inner: RefCell::new(CacheInner {
                map: HashMap::new(),
                order: Vec::new(),
                capacity: 2,
            }),
        };
        cache.insert(make_id("a"), make_pixels(1), 1, 1);
        cache.insert(make_id("b"), make_pixels(2), 1, 1);
        cache.insert(make_id("c"), make_pixels(3), 1, 1);

        assert!(cache.get(&make_id("a")).is_none(), "a should be evicted");
        assert!(cache.get(&make_id("b")).is_some());
        assert!(cache.get(&make_id("c")).is_some());
    }

    #[test]
    fn access_promotes_to_mru() {
        let cache = TextureCache {
            inner: RefCell::new(CacheInner {
                map: HashMap::new(),
                order: Vec::new(),
                capacity: 2,
            }),
        };
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

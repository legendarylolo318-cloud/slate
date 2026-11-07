//! Thumbnail cache: bounded in-memory LRU over the core's encrypted disk cache.
//!
//! Network fetch and decode run off the UI thread; concurrent requests for the
//! same URL share one fetch. Callers get a `SharedPixelBuffer`, which is cheap
//! to clone and `Send`; the `slint::Image` is built on the UI thread.
//!
//! [`ImageCache::get_fit`] decodes at the size an image is shown at (times the
//! window's scale factor); each size is its own entry, keyed `url@px`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::future::{BoxFuture, FutureExt, Shared};
use lru::LruCache;
use slate_core::commands::utility;
use slate_core::AppState;
use tokio::sync::Semaphore;

use crate::images;
pub use crate::images::Pixels;

/// In-memory budget for decoded pixels.
/// (~650 row thumbnails at 160 px; list models keep only the covers near the
/// viewport, see `models::MAX_COVER_PAGES`.)
const MEMORY_BUDGET: usize = 64 * 1024 * 1024;
/// Concurrent network fetches.
const MAX_FETCHES: usize = 8;

type Pending = Shared<BoxFuture<'static, Option<Pixels>>>;

/// A cache key: the URL and the edge it was decoded at, ranked so that "original size"
/// (`max_edge` 0) sorts above every number.
type Key = (String, u32);

/// `max_edge` as a rank: 0 (original size) is the largest.
fn rank(max_edge: u32) -> u32 {
    if max_edge == 0 {
        u32::MAX
    } else {
        max_edge
    }
}

struct Lru {
    map: LruCache<Key, Pixels>,
    bytes: usize,
    /// The ranks cached per URL, so a smaller size can be cut from a bigger one already
    /// decoded. Kept in step with `map` (insert, replace, evict).
    sizes: HashMap<String, Vec<u32>>,
}

impl Lru {
    fn forget(&mut self, key: &Key, px: &Pixels) {
        self.bytes = self.bytes.saturating_sub(px.byte_size());
        if let Some(v) = self.sizes.get_mut(&key.0) {
            v.retain(|r| *r != key.1);
            if v.is_empty() {
                self.sizes.remove(&key.0);
            }
        }
    }

    /// The entry for `url` at exactly `rank`, else the smallest bigger one.
    fn find(&mut self, url: &str, rank: u32) -> Option<(Pixels, bool)> {
        if let Some(px) = self.map.get(&(url.to_string(), rank)) {
            return Some((px.clone(), true));
        }
        let best = self
            .sizes
            .get(url)?
            .iter()
            .copied()
            .filter(|r| *r > rank)
            .min()?;
        self.map
            .get(&(url.to_string(), best))
            .map(|px| (px.clone(), false))
    }
}

pub struct ImageCache {
    lru: Mutex<Lru>,
    inflight: Mutex<HashMap<Key, Pending>>,
    net: Arc<Semaphore>,
    /// Window scale factor (`f32` bits), for [`ImageCache::get_fit`].
    scale: AtomicU32,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            lru: Mutex::new(Lru {
                map: LruCache::unbounded(),
                bytes: 0,
                sizes: HashMap::new(),
            }),
            inflight: Mutex::new(HashMap::new()),
            net: Arc::new(Semaphore::new(MAX_FETCHES)),
            scale: AtomicU32::new(1.0f32.to_bits()),
        }
    }
}

impl ImageCache {
    /// Records the window's scale factor (call from the UI thread on changes).
    pub fn set_scale(&self, scale: f32) {
        if scale.is_finite() && scale > 0.0 {
            self.scale.store(scale.to_bits(), Ordering::Relaxed);
        }
    }

    /// The full-size image.
    pub async fn get(&self, state: &Arc<AppState>, url: &str) -> Option<Pixels> {
        self.fetch(state, url, 0).await
    }

    /// The image shrunk to fit a `side` × `side` logical-pixel box.
    pub async fn get_fit(&self, state: &Arc<AppState>, url: &str, side: u32) -> Option<Pixels> {
        let scale = f32::from_bits(self.scale.load(Ordering::Relaxed));
        // Saturating float → int; a nonsensical scale falls back to full size.
        let px = (side as f32 * scale).ceil() as u32;
        self.fetch(state, url, px).await
    }

    /// `max_edge` 0 = original size.
    ///
    /// A cover is read from the disk cache and decoded once per URL and size *class*: a size
    /// is cut from a bigger one that is already decoded (or being decoded) instead of reading
    /// and decoding the file again.
    async fn fetch(&self, state: &Arc<AppState>, url: &str, max_edge: u32) -> Option<Pixels> {
        if url.is_empty() {
            return None;
        }
        let want = rank(max_edge);
        let cached = self.lru.lock().ok().and_then(|mut l| l.find(url, want));
        match cached {
            Some((px, true)) => return Some(px),
            Some((big, false)) => return self.cut(url, max_edge, big).await,
            None => {}
        }
        let (pending, exact) = {
            let mut inflight = self.inflight.lock().ok()?;
            let key = (url.to_string(), want);
            let bigger = inflight
                .iter()
                .filter(|((u, r), _)| u == url && *r > want)
                .min_by_key(|((_, r), _)| *r)
                .map(|(_, p)| p.clone());
            match (inflight.get(&key).cloned(), bigger) {
                (Some(p), _) => (p, true),
                (None, Some(p)) => (p, false),
                (None, None) => {
                    let (state, url, net) =
                        (Arc::clone(state), url.to_string(), Arc::clone(&self.net));
                    let p: Pending = async move {
                        let _permit = net.acquire().await.ok()?;
                        let bytes = utility::get_image_bytes(&state, url).await.ok()?;
                        tokio::task::spawn_blocking(move || images::decode_pixels(&bytes, max_edge))
                            .await
                            .ok()
                            .flatten()
                    }
                    .boxed()
                    .shared();
                    inflight.insert(key, p.clone());
                    (p, true)
                }
            }
        };
        let result = pending.clone().await;
        if !exact {
            // Someone else's bigger fetch: cut ours from its result.
            return match result {
                Some(big) => self.cut(url, max_edge, big).await,
                None => None,
            };
        }
        // Cache before leaving the in-flight map, so a request arriving in between
        // finds the pixels instead of starting a second fetch; and only remove our
        // own entry, not a newer fetch for the same URL.
        if let Some(px) = &result {
            self.insert(url, want, px.clone());
        }
        if let Ok(mut inflight) = self.inflight.lock() {
            let key = (url.to_string(), want);
            if inflight.get(&key).is_some_and(|p| p.ptr_eq(&pending)) {
                inflight.remove(&key);
            }
        }
        result
    }

    /// `big` (a decoded size of `url`) shrunk to `max_edge` and cached. `big` itself when it
    /// is not larger than that.
    async fn cut(&self, url: &str, max_edge: u32, big: Pixels) -> Option<Pixels> {
        if max_edge == 0 || big.width().max(big.height()) <= max_edge {
            return Some(big);
        }
        let small = tokio::task::spawn_blocking(move || big.fit(max_edge))
            .await
            .ok()?;
        self.insert(url, rank(max_edge), small.clone());
        Some(small)
    }

    fn insert(&self, url: &str, rank: u32, px: Pixels) {
        let Ok(mut l) = self.lru.lock() else { return };
        let key = (url.to_string(), rank);
        l.bytes += px.byte_size();
        let ranks = l.sizes.entry(url.to_string()).or_default();
        if !ranks.contains(&rank) {
            ranks.push(rank);
        }
        if let Some(old) = l.map.put(key.clone(), px) {
            l.bytes = l.bytes.saturating_sub(old.byte_size());
        }
        while l.bytes > MEMORY_BUDGET {
            match l.map.pop_lru() {
                Some((k, evicted)) => l.forget(&k, &evicted),
                None => break,
            }
        }
    }

    #[cfg(test)]
    fn resident_bytes(&self) -> usize {
        self.lru.lock().map(|l| l.bytes).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::{Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer};

    fn rgba(w: u32, h: u32) -> Pixels {
        Pixels::Rgba(SharedPixelBuffer::<Rgba8Pixel>::new(w, h))
    }

    fn rgb(w: u32, h: u32) -> Pixels {
        Pixels::Rgb(SharedPixelBuffer::<Rgb8Pixel>::new(w, h))
    }

    #[test]
    fn evicts_least_recently_used_beyond_budget() {
        let cache = ImageCache::default();
        // 2048x2048 RGBA = 16 MiB each; the budget holds four.
        for i in 0..8 {
            cache.insert(&format!("u{i}"), rank(0), rgba(2048, 2048));
        }
        assert!(cache.resident_bytes() <= MEMORY_BUDGET);
        let l = cache.lru.lock().expect("lock");
        assert!(l.map.contains(&("u7".to_string(), rank(0))));
        assert!(!l.map.contains(&("u0".to_string(), rank(0))));
        assert!(!l.sizes.contains_key("u0"), "an evicted size is forgotten");
        assert!(l.sizes.contains_key("u7"));
    }

    #[test]
    fn replacing_an_entry_does_not_double_count() {
        let cache = ImageCache::default();
        cache.insert("a", 160, rgba(10, 10));
        cache.insert("a", 160, rgba(10, 10));
        assert_eq!(cache.resident_bytes(), 400);
        assert_eq!(cache.lru.lock().expect("lock").sizes["a"], vec![160]);
    }

    #[test]
    fn rgb_covers_count_three_bytes_a_pixel() {
        let cache = ImageCache::default();
        cache.insert("a", 160, rgb(10, 10));
        assert_eq!(cache.resident_bytes(), 300);
        // The budget holds a third more RGB covers than RGBA ones.
        let per = |p: &Pixels| p.byte_size();
        assert_eq!(per(&rgb(640, 640)) * 4, per(&rgba(640, 640)) * 3);
        let cache = ImageCache::default();
        let mut n = 0;
        while cache.resident_bytes() + 640 * 640 * 3 <= MEMORY_BUDGET {
            cache.insert(&format!("c{n}"), 640, rgb(640, 640));
            n += 1;
        }
        assert_eq!(n, MEMORY_BUDGET / (640 * 640 * 3));
        assert!(cache.resident_bytes() <= MEMORY_BUDGET);
    }

    #[test]
    fn a_smaller_size_is_found_in_a_bigger_cached_one() {
        let cache = ImageCache::default();
        cache.insert("u", 320, rgb(320, 320));
        cache.insert("u", 640, rgb(640, 640));
        cache.insert("other", 640, rgb(640, 640));
        let mut l = cache.lru.lock().expect("lock");
        let (px, exact) = l.find("u", rank(160)).expect("a bigger one");
        assert!(!exact);
        assert_eq!(px.width(), 320, "the smallest bigger entry is cut from");
        assert!(l.find("u", rank(320)).is_some_and(|(_, e)| e));
        assert!(
            l.find("u", rank(0)).is_none(),
            "original size needs its own decode"
        );
        assert!(l.find("u", rank(700)).is_none());
        assert!(l.find("nope", rank(10)).is_none());
    }

    #[tokio::test]
    async fn cutting_caches_the_smaller_size_and_never_enlarges() {
        let cache = ImageCache::default();
        let big = rgb(320, 320);
        let small = cache.cut("u", 160, big.clone()).await.expect("cut");
        assert_eq!((small.width(), small.height()), (160, 160));
        assert!(matches!(small, Pixels::Rgb(_)), "stays RGB");
        assert_eq!(cache.resident_bytes(), 160 * 160 * 3);
        let same = cache.cut("u", 640, big).await.expect("cut");
        assert_eq!(
            same.width(),
            320,
            "asked for more than there is: the image as it is"
        );
        assert_eq!(cache.resident_bytes(), 160 * 160 * 3, "nothing new cached");
    }
}

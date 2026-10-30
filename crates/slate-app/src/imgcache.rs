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
use slint::{Rgba8Pixel, SharedPixelBuffer};
use tokio::sync::Semaphore;

use crate::images;

/// In-memory budget for decoded pixels.
/// (~650 row thumbnails at 160 px; list models keep only the covers near the
/// viewport, see `models::MAX_COVER_PAGES`.)
const MEMORY_BUDGET: usize = 64 * 1024 * 1024;
/// Concurrent network fetches.
const MAX_FETCHES: usize = 8;

pub type Pixels = SharedPixelBuffer<Rgba8Pixel>;
type Pending = Shared<BoxFuture<'static, Option<Pixels>>>;

struct Lru {
    map: LruCache<String, Pixels>,
    bytes: usize,
}

pub struct ImageCache {
    lru: Mutex<Lru>,
    inflight: Mutex<HashMap<String, Pending>>,
    net: Arc<Semaphore>,
    /// Window scale factor (`f32` bits), for [`ImageCache::get_fit`].
    scale: AtomicU32,
}

fn size_of(p: &Pixels) -> usize {
    p.width() as usize * p.height() as usize * 4
}

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            lru: Mutex::new(Lru {
                map: LruCache::unbounded(),
                bytes: 0,
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
    async fn fetch(&self, state: &Arc<AppState>, url: &str, max_edge: u32) -> Option<Pixels> {
        if url.is_empty() {
            return None;
        }
        let key = if max_edge == 0 {
            url.to_string()
        } else {
            format!("{url}@{max_edge}")
        };
        let key = key.as_str();
        if let Some(hit) = self
            .lru
            .lock()
            .ok()
            .and_then(|mut l| l.map.get(key).cloned())
        {
            return Some(hit);
        }
        let pending = {
            let mut inflight = self.inflight.lock().ok()?;
            inflight
                .entry(key.to_string())
                .or_insert_with(|| {
                    let (state, url, net) =
                        (Arc::clone(state), url.to_string(), Arc::clone(&self.net));
                    async move {
                        let _permit = net.acquire().await.ok()?;
                        let bytes = utility::get_image_bytes(&state, url).await.ok()?;
                        tokio::task::spawn_blocking(move || images::decode_fit(&bytes, max_edge))
                            .await
                            .ok()
                            .flatten()
                    }
                    .boxed()
                    .shared()
                })
                .clone()
        };
        let result = pending.clone().await;
        // Cache before leaving the in-flight map, so a request arriving in between
        // finds the pixels instead of starting a second fetch; and only remove our
        // own entry, not a newer fetch for the same URL.
        if let Some(px) = &result {
            self.insert(key, px.clone());
        }
        if let Ok(mut inflight) = self.inflight.lock() {
            if inflight.get(key).is_some_and(|p| p.ptr_eq(&pending)) {
                inflight.remove(key);
            }
        }
        result
    }

    fn insert(&self, url: &str, px: Pixels) {
        let Ok(mut l) = self.lru.lock() else { return };
        l.bytes += size_of(&px);
        if let Some(old) = l.map.put(url.to_string(), px) {
            l.bytes = l.bytes.saturating_sub(size_of(&old));
        }
        while l.bytes > MEMORY_BUDGET {
            match l.map.pop_lru() {
                Some((_, evicted)) => l.bytes = l.bytes.saturating_sub(size_of(&evicted)),
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

    #[test]
    fn evicts_least_recently_used_beyond_budget() {
        let cache = ImageCache::default();
        // 2048x2048 RGBA = 16 MiB each; the budget holds six.
        for i in 0..8 {
            cache.insert(&format!("u{i}"), Pixels::new(2048, 2048));
        }
        assert!(cache.resident_bytes() <= MEMORY_BUDGET);
        let l = cache.lru.lock().expect("lock");
        assert!(l.map.contains("u7"));
        assert!(!l.map.contains("u0"));
    }

    #[test]
    fn replacing_an_entry_does_not_double_count() {
        let cache = ImageCache::default();
        cache.insert("a", Pixels::new(10, 10));
        cache.insert("a", Pixels::new(10, 10));
        assert_eq!(cache.resident_bytes(), 400);
    }
}

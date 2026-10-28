//! Thumbnail cache: bounded in-memory LRU over the core's encrypted disk cache.
//!
//! Network fetch and decode run off the UI thread; concurrent requests for the
//! same URL share one fetch. Callers get a `SharedPixelBuffer`, which is cheap
//! to clone and `Send`; the `slint::Image` is built on the UI thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::future::{BoxFuture, FutureExt, Shared};
use lru::LruCache;
use slate_core::commands::utility;
use slate_core::AppState;
use slint::{Rgba8Pixel, SharedPixelBuffer};
use tokio::sync::Semaphore;

use crate::images;

/// In-memory budget for decoded pixels.
const MEMORY_BUDGET: usize = 96 * 1024 * 1024;
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
        }
    }
}

impl ImageCache {
    pub async fn get(&self, state: &Arc<AppState>, url: &str) -> Option<Pixels> {
        if url.is_empty() {
            return None;
        }
        if let Some(hit) = self
            .lru
            .lock()
            .ok()
            .and_then(|mut l| l.map.get(url).cloned())
        {
            return Some(hit);
        }
        let pending = {
            let mut inflight = self.inflight.lock().ok()?;
            inflight
                .entry(url.to_string())
                .or_insert_with(|| {
                    let (state, url, net) =
                        (Arc::clone(state), url.to_string(), Arc::clone(&self.net));
                    async move {
                        let _permit = net.acquire().await.ok()?;
                        let bytes = utility::get_image_bytes(&state, url).await.ok()?;
                        tokio::task::spawn_blocking(move || images::decode(&bytes))
                            .await
                            .ok()
                            .flatten()
                    }
                    .boxed()
                    .shared()
                })
                .clone()
        };
        let result = pending.await;
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.remove(url);
        }
        if let Some(px) = &result {
            self.insert(url, px.clone());
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

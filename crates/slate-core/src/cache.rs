use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::crypto::Crypto;
use crate::now_secs;

// ---------------------------------------------------------------------------
// CacheTier
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CacheTier {
    /// Playlists, liked tracks, favorites.  TTL: 15 min.
    UserContent,
    /// Artist bios, top charts, home page.  TTL: 4 hours.
    Dynamic,
    /// Album tracklists, credits.           TTL: 7 days.
    StaticMeta,
    /// Album art, avatars.                  TTL: 30 days.
    Image,
}

impl CacheTier {
    pub fn ttl(&self) -> Duration {
        match self {
            CacheTier::UserContent => Duration::from_secs(15 * 60),
            CacheTier::Dynamic => Duration::from_secs(4 * 60 * 60),
            CacheTier::StaticMeta => Duration::from_secs(7 * 24 * 60 * 60),
            CacheTier::Image => Duration::from_secs(30 * 24 * 60 * 60),
        }
    }

    /// How long past TTL we still serve stale data while refreshing.
    pub fn swr_grace(&self) -> Duration {
        match self {
            CacheTier::UserContent => Duration::from_secs(60 * 60),
            CacheTier::Dynamic => Duration::from_secs(24 * 60 * 60),
            CacheTier::StaticMeta => Duration::from_secs(30 * 24 * 60 * 60),
            CacheTier::Image => Duration::from_secs(90 * 24 * 60 * 60),
        }
    }

    fn subdir(&self) -> &'static str {
        match self {
            CacheTier::UserContent => "user",
            CacheTier::Dynamic => "dynamic",
            CacheTier::StaticMeta => "static",
            CacheTier::Image => "images",
        }
    }
}

const ALL_TIERS: [CacheTier; 4] = [
    CacheTier::UserContent,
    CacheTier::Dynamic,
    CacheTier::StaticMeta,
    CacheTier::Image,
];

// ---------------------------------------------------------------------------
// Cache statistics (public API)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    pub total_entries: usize,
    pub total_disk_mb: f64,
    pub max_disk_mb: f64,
    pub usage_percent: f64,
    pub by_tier: HashMap<String, TierStats>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TierStats {
    pub count: usize,
    pub size_mb: f64,
}

// ---------------------------------------------------------------------------
// CacheResult
// ---------------------------------------------------------------------------

pub enum CacheResult {
    /// Data is within TTL — no refresh needed.
    Fresh(Vec<u8>),
    /// Data is past TTL but within SWR grace — caller should return this AND
    /// trigger a background refresh.
    Stale(Vec<u8>),
    /// Not found or expired beyond SWR grace.
    Miss,
}

// ---------------------------------------------------------------------------
// On-disk metadata per entry
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
struct EntryMeta {
    schema_version: u8,
    tags: Vec<String>,
    tier: CacheTier,
    created_at: u64,
    size: u64,
}

// ---------------------------------------------------------------------------
// In-memory index entry
// ---------------------------------------------------------------------------

struct IndexEntry {
    tier: CacheTier,
    #[allow(dead_code)] // kept for index rebuild from metadata files
    tags: Vec<String>,
    created_at: u64,
    size: u64,
    last_access: u64,
    last_refresh_attempt: Option<u64>,
}

// ---------------------------------------------------------------------------
// Inner state (behind RwLock)
// ---------------------------------------------------------------------------

struct DiskCacheInner {
    /// hash → index entry
    index: HashMap<String, IndexEntry>,
    /// tag → set of hashes
    tag_index: HashMap<String, Vec<String>>,
    /// Monotonic counter for LRU ordering.
    access_counter: u64,
    /// Total bytes of all `.dat` files on disk.
    total_disk_usage: u64,
    /// Keys currently being refreshed (prevents duplicate SWR spawns), with
    /// the invalidation epoch the refresh started in.
    in_flight: HashMap<String, u64>,
    /// Bumped by every invalidation and `clear`. A refresh that started in an
    /// older epoch may have fetched what was just invalidated, so its write is
    /// dropped (see `put`).
    epoch: u64,
}

impl DiskCacheInner {
    fn new() -> Self {
        Self {
            index: HashMap::new(),
            tag_index: HashMap::new(),
            access_counter: 0,
            total_disk_usage: 0,
            in_flight: HashMap::new(),
            epoch: 0,
        }
    }

    fn add_to_tag_index(&mut self, hash: &str, tags: &[String]) {
        for tag in tags {
            self.tag_index
                .entry(tag.clone())
                .or_default()
                .push(hash.to_string());
        }
    }

    fn remove_from_tag_index(&mut self, hash: &str) {
        // Remove hash from every tag list, clean up empty lists.
        self.tag_index.retain(|_, hashes| {
            hashes.retain(|h| h != hash);
            !hashes.is_empty()
        });
    }

    /// True when `hash` is being refreshed by a task that started before the
    /// latest invalidation.
    fn refresh_outdated(&self, hash: &str) -> bool {
        self.in_flight
            .get(hash)
            .is_some_and(|started| *started != self.epoch)
    }

    fn remove_entry(&mut self, hash: &str) -> Option<IndexEntry> {
        if let Some(entry) = self.index.remove(hash) {
            self.total_disk_usage = self.total_disk_usage.saturating_sub(entry.size);
            self.remove_from_tag_index(hash);
            Some(entry)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// DiskCache
// ---------------------------------------------------------------------------

const MAX_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GB
const EVICT_TARGET: u64 = MAX_DISK_BYTES * 9 / 10; // 1.8 GB
const CURRENT_SCHEMA_VERSION: u8 = 7;

pub struct DiskCache {
    base_dir: PathBuf,
    inner: RwLock<DiskCacheInner>,
    crypto: Arc<Crypto>,
}

impl DiskCache {
    /// Create cache and rebuild the in-memory index by scanning disk.
    /// The actual storage lives under `cache_dir/v{CURRENT_SCHEMA_VERSION}/`.
    pub fn new(cache_dir: &Path, crypto: Arc<Crypto>) -> Self {
        let base_dir = cache_dir.join(format!("v{CURRENT_SCHEMA_VERSION}"));

        // Delete old version folders.
        if let Ok(entries) = fs::read_dir(cache_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                if name.starts_with('v') && name != format!("v{CURRENT_SCHEMA_VERSION}") {
                    log::info!("[DiskCache] removing old cache folder: {name}");
                    fs::remove_dir_all(&path).ok();
                }
            }
        }

        // Ensure tier subdirs exist.
        for tier in &ALL_TIERS {
            fs::create_dir_all(base_dir.join(tier.subdir())).ok();
        }

        let mut inner = DiskCacheInner::new();
        let now = now_secs();

        // Scan each tier subdir for .meta files and rebuild the index.
        for tier in &ALL_TIERS {
            let dir = base_dir.join(tier.subdir());
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let path = entry.path();
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };

                // Only process .meta files
                if !name.ends_with(".meta") {
                    continue;
                }
                let hash = name.trim_end_matches(".meta").to_string();
                let dat_path = dir.join(format!("{}.dat", hash));

                // Read and parse meta
                let meta: EntryMeta = match fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                {
                    Some(m) => m,
                    None => {
                        // Corrupt meta — delete both files.
                        fs::remove_file(&path).ok();
                        fs::remove_file(&dat_path).ok();
                        continue;
                    }
                };

                // Check schema version — delete if mismatch.
                if meta.schema_version != CURRENT_SCHEMA_VERSION {
                    log::debug!(
                        "[DiskCache] schema mismatch: {} (expected {}), deleting {}",
                        meta.schema_version,
                        CURRENT_SCHEMA_VERSION,
                        hash
                    );
                    fs::remove_file(&path).ok();
                    fs::remove_file(&dat_path).ok();
                    continue;
                }

                // Check if entry is beyond TTL + SWR grace (fully expired).
                let max_age = meta.tier.ttl().as_secs() + meta.tier.swr_grace().as_secs();
                let age = now.saturating_sub(meta.created_at);
                if age > max_age {
                    fs::remove_file(&path).ok();
                    fs::remove_file(&dat_path).ok();
                    continue;
                }

                // Verify dat file exists
                if !dat_path.exists() {
                    fs::remove_file(&path).ok();
                    continue;
                }

                inner.access_counter += 1;
                inner.total_disk_usage += meta.size;
                inner.add_to_tag_index(&hash, &meta.tags);
                inner.index.insert(
                    hash,
                    IndexEntry {
                        tier: meta.tier,
                        tags: meta.tags,
                        created_at: meta.created_at,
                        size: meta.size,
                        last_access: inner.access_counter,
                        last_refresh_attempt: None,
                    },
                );
            }
        }

        log::info!(
            "[DiskCache] rebuilt index: {} entries, {:.1} MB on disk",
            inner.index.len(),
            inner.total_disk_usage as f64 / (1024.0 * 1024.0)
        );

        Self {
            base_dir,
            inner: RwLock::new(inner),
            crypto,
        }
    }

    /// Look up a cache entry. Returns Fresh / Stale / Miss.
    pub async fn get(&self, key: &str, tier: CacheTier) -> CacheResult {
        let hash = hash_key(key);

        // Read-lock: check index.
        let (created_at, exists) = {
            let inner = self.inner.read().await;
            match inner.index.get(&hash) {
                Some(entry) => (entry.created_at, true),
                None => (0, false),
            }
        };

        if !exists {
            log::debug!("[DiskCache] MISS: {} (tier={:?})", key, tier);
            return CacheResult::Miss;
        }

        // Read data from disk (outside lock) and decrypt, off the async workers.
        let dat_path = self
            .base_dir
            .join(tier.subdir())
            .join(format!("{}.dat", hash));
        let crypto = Arc::clone(&self.crypto);
        let read = tokio::task::spawn_blocking(move || {
            fs::read(&dat_path).map(|raw| crypto.decrypt(&raw))
        })
        .await;
        let data = match read {
            Ok(Ok(Ok(plain))) => plain,
            Ok(Ok(Err(e))) => {
                // Decryption failed (key changed?) — treat as miss, remove corrupt entry.
                log::warn!("[DiskCache] decrypt failed for {}: {e}", &hash[..12]);
                self.remove_files_blocking(vec![(hash.clone(), tier)]).await;
                let mut inner = self.inner.write().await;
                inner.remove_entry(&hash);
                return CacheResult::Miss;
            }
            Ok(Err(_)) | Err(_) => {
                // File gone but index has it — clean up.
                let mut inner = self.inner.write().await;
                inner.remove_entry(&hash);
                return CacheResult::Miss;
            }
        };

        // Update LRU access counter.
        {
            let mut inner = self.inner.write().await;
            inner.access_counter += 1;
            let counter = inner.access_counter;
            if let Some(entry) = inner.index.get_mut(&hash) {
                entry.last_access = counter;
            }
        }

        let age = now_secs().saturating_sub(created_at);
        let ttl = tier.ttl().as_secs();
        let grace = tier.swr_grace().as_secs();

        if age < ttl {
            log::debug!(
                "[DiskCache] HIT (fresh): {} (tier={:?}, age={}s)",
                key,
                tier,
                age
            );
            CacheResult::Fresh(data)
        } else if age < ttl + grace {
            log::debug!(
                "[DiskCache] HIT (stale): {} (tier={:?}, age={}s, refresh needed)",
                key,
                tier,
                age
            );
            CacheResult::Stale(data)
        } else {
            log::debug!(
                "[DiskCache] MISS (expired): {} (tier={:?}, age={}s)",
                key,
                tier,
                age
            );
            // Beyond grace — treat as miss and clean up.
            self.remove_files_blocking(vec![(hash.clone(), tier)]).await;
            let mut inner = self.inner.write().await;
            inner.remove_entry(&hash);
            CacheResult::Miss
        }
    }

    /// Write data to cache. Triggers LRU eviction if over 2 GB.
    pub async fn put(
        &self,
        key: &str,
        data: &[u8],
        tier: CacheTier,
        tags: &[&str],
    ) -> Result<(), std::io::Error> {
        let hash = hash_key(key);
        let size = data.len() as u64;
        let tag_strings: Vec<String> = tags.iter().map(|s| s.to_string()).collect();

        let dir = self.base_dir.join(tier.subdir());
        let dat_path = dir.join(format!("{}.dat", hash));
        let meta_path = dir.join(format!("{}.meta", hash));

        if self.inner.read().await.refresh_outdated(&hash) {
            log::debug!("[DiskCache] dropped outdated refresh for {}", &hash[..12]);
            return Ok(());
        }

        // Encrypt and write both files off the async workers.
        let meta = EntryMeta {
            schema_version: CURRENT_SCHEMA_VERSION,
            tags: tag_strings.clone(),
            tier,
            created_at: now_secs(),
            size,
        };
        let crypto = Arc::clone(&self.crypto);
        let plain = data.to_vec();
        tokio::task::spawn_blocking(move || -> Result<(), std::io::Error> {
            let encrypted = crypto
                .encrypt(&plain)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            fs::write(&dat_path, &encrypted)?;
            fs::write(&meta_path, serde_json::to_string(&meta).unwrap_or_default())
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))??;

        // Update index.
        {
            let mut inner = self.inner.write().await;

            // Invalidated while the files were being written: what was
            // written is outdated, so forget the key rather than index it.
            if inner.refresh_outdated(&hash) {
                inner.remove_entry(&hash);
                drop(inner);
                self.remove_files_blocking(vec![(hash, tier)]).await;
                return Ok(());
            }

            // Remove old entry if overwriting.
            if let Some(old) = inner.index.remove(&hash) {
                inner.total_disk_usage = inner.total_disk_usage.saturating_sub(old.size);
                inner.remove_from_tag_index(&hash);
            }

            inner.access_counter += 1;
            let counter = inner.access_counter;
            inner.total_disk_usage += size;
            inner.add_to_tag_index(&hash, &tag_strings);
            inner.index.insert(
                hash,
                IndexEntry {
                    tier,
                    tags: tag_strings,
                    created_at: now_secs(),
                    size,
                    last_access: counter,
                    last_refresh_attempt: None,
                },
            );
        }

        // Evict if over limit.
        self.maybe_evict().await;

        Ok(())
    }

    /// Invalidate all entries matching a given tag.
    pub async fn invalidate_tag(&self, tag: &str) {
        let hashes: Vec<String> = {
            let mut inner = self.inner.write().await;
            inner.epoch += 1;
            inner.tag_index.get(tag).cloned().unwrap_or_default()
        };

        if hashes.is_empty() {
            return;
        }

        let doomed = {
            let mut inner = self.inner.write().await;
            let mut doomed = Vec::new();
            for hash in &hashes {
                if let Some(entry) = inner.index.get(hash) {
                    doomed.push((hash.clone(), entry.tier));
                }
                inner.remove_entry(hash);
            }
            doomed
        };
        self.remove_files_blocking(doomed).await;
    }

    /// Invalidate a single entry by its plaintext key.
    pub async fn invalidate_key(&self, key: &str) {
        let hash = hash_key(key);
        let doomed = {
            let mut inner = self.inner.write().await;
            inner.epoch += 1;
            let tier = inner.index.get(&hash).map(|e| e.tier);
            inner.remove_entry(&hash);
            tier.map(|t| (hash, t))
        };
        self.remove_files_blocking(doomed.into_iter().collect())
            .await;
    }

    /// Clear the entire cache (e.g. on logout).
    pub async fn clear(&self) {
        let mut inner = self.inner.write().await;
        inner.index.clear();
        inner.tag_index.clear();
        inner.total_disk_usage = 0;
        inner.access_counter = 0;
        // Refreshes still running keep their markers (they clear them when
        // done); the new epoch makes their writes drop.
        inner.epoch += 1;
        drop(inner);

        // Delete all files in tier subdirs.
        let dirs: Vec<PathBuf> = ALL_TIERS
            .iter()
            .map(|tier| self.base_dir.join(tier.subdir()))
            .collect();
        tokio::task::spawn_blocking(move || {
            for dir in dirs {
                if let Ok(entries) = fs::read_dir(&dir) {
                    for entry in entries.flatten() {
                        fs::remove_file(entry.path()).ok();
                    }
                }
            }
        })
        .await
        .ok();
    }

    /// Mark a key as in-flight (being refreshed). Returns `true` if it was
    /// not already in-flight (caller should proceed with refresh).
    pub async fn mark_in_flight(&self, key: &str) -> bool {
        let hash = hash_key(key);
        let mut inner = self.inner.write().await;
        let epoch = inner.epoch;
        match inner.in_flight.entry(hash) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(epoch);
                true
            }
        }
    }

    /// Remove in-flight marker after refresh completes.
    pub async fn clear_in_flight(&self, key: &str) {
        let hash = hash_key(key);
        let mut inner = self.inner.write().await;
        inner.in_flight.remove(&hash);
    }

    /// Check if enough time has passed since last refresh attempt.
    /// Returns true if we should retry (no recent attempt OR >min_interval elapsed).
    pub async fn should_retry_refresh(&self, key: &str, min_interval_secs: u64) -> bool {
        let hash = hash_key(key);
        let inner = self.inner.read().await;

        if let Some(entry) = inner.index.get(&hash) {
            if let Some(last_attempt) = entry.last_refresh_attempt {
                let elapsed = now_secs().saturating_sub(last_attempt);
                elapsed >= min_interval_secs
            } else {
                true // Never attempted
            }
        } else {
            true // Entry doesn't exist
        }
    }

    /// Mark that we're attempting a refresh (updates timestamp).
    pub async fn mark_refresh_attempt(&self, key: &str) {
        let hash = hash_key(key);
        let mut inner = self.inner.write().await;
        if let Some(entry) = inner.index.get_mut(&hash) {
            entry.last_refresh_attempt = Some(now_secs());
        }
    }

    /// Get cache statistics for monitoring/debugging.
    pub async fn stats(&self) -> CacheStats {
        let inner = self.inner.read().await;

        let mut by_tier: HashMap<String, TierStats> = HashMap::new();
        for entry in inner.index.values() {
            let tier_name = format!("{:?}", entry.tier);
            let stats = by_tier.entry(tier_name).or_insert(TierStats {
                count: 0,
                size_mb: 0.0,
            });
            stats.count += 1;
            stats.size_mb += entry.size as f64 / (1024.0 * 1024.0);
        }

        let total_disk_mb = inner.total_disk_usage as f64 / (1024.0 * 1024.0);
        let max_disk_mb = MAX_DISK_BYTES as f64 / (1024.0 * 1024.0);

        CacheStats {
            total_entries: inner.index.len(),
            total_disk_mb,
            max_disk_mb,
            usage_percent: (total_disk_mb / max_disk_mb) * 100.0,
            by_tier,
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Delete the files of these entries on a blocking thread. The index has
    /// already forgotten them, so a failure only leaves an orphan file that
    /// the next start's scan cleans up.
    async fn remove_files_blocking(&self, entries: Vec<(String, CacheTier)>) {
        if entries.is_empty() {
            return;
        }
        let base = self.base_dir.clone();
        tokio::task::spawn_blocking(move || {
            for (hash, tier) in entries {
                let dir = base.join(tier.subdir());
                fs::remove_file(dir.join(format!("{}.dat", hash))).ok();
                fs::remove_file(dir.join(format!("{}.meta", hash))).ok();
            }
        })
        .await
        .ok();
    }

    async fn maybe_evict(&self) {
        let usage = {
            let inner = self.inner.read().await;
            inner.total_disk_usage
        };

        if usage <= MAX_DISK_BYTES {
            return;
        }

        log::info!(
            "[DiskCache] evicting: {:.1} MB > {:.1} MB limit",
            usage as f64 / (1024.0 * 1024.0),
            MAX_DISK_BYTES as f64 / (1024.0 * 1024.0)
        );

        let doomed = {
            let mut inner = self.inner.write().await;

            // Collect entries sorted by last_access ascending (oldest first).
            let mut entries: Vec<(String, u64, u64, CacheTier)> = inner
                .index
                .iter()
                .map(|(hash, e)| (hash.clone(), e.last_access, e.size, e.tier))
                .collect();
            entries.sort_by_key(|(_, access, _, _)| *access);

            let mut doomed = Vec::new();
            for (hash, _, size, tier) in entries {
                if inner.total_disk_usage <= EVICT_TARGET {
                    break;
                }
                inner.remove_entry(&hash);
                log::debug!(
                    "[DiskCache] evicted {} ({} bytes, {:?})",
                    &hash[..12],
                    size,
                    tier
                );
                doomed.push((hash, tier));
            }
            doomed
        };
        self.remove_files_blocking(doomed).await;
    }
}

// ---------------------------------------------------------------------------
// Key hashing
// ---------------------------------------------------------------------------

fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod schema_tests {
    #[test]
    fn schema_bumped_for_ai_field() {
        const { assert!(super::CURRENT_SCHEMA_VERSION >= 6) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(dir: &Path) -> DiskCache {
        DiskCache::new(dir, Arc::new(Crypto::with_test_key()))
    }

    #[tokio::test]
    async fn put_then_get_round_trips_through_encryption() {
        let dir = tempfile::tempdir().expect("tempdir");
        let c = cache(dir.path());
        c.put("k", b"hello", CacheTier::Dynamic, &["t"])
            .await
            .expect("put");
        match c.get("k", CacheTier::Dynamic).await {
            CacheResult::Fresh(b) => assert_eq!(b, b"hello"),
            _ => panic!("expected a fresh hit"),
        }
        // Nothing readable on disk.
        let raw = fs::read(dir.path().join(format!(
            "v{CURRENT_SCHEMA_VERSION}/dynamic/{}.dat",
            hash_key("k")
        )))
        .expect("dat file");
        assert!(!raw.windows(5).any(|w| w == b"hello"));
        assert!(matches!(
            c.get("other", CacheTier::Dynamic).await,
            CacheResult::Miss
        ));
    }

    #[tokio::test]
    async fn invalidate_by_tag_and_key_removes_entries_and_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let c = cache(dir.path());
        c.put("a", b"1", CacheTier::UserContent, &["album"])
            .await
            .expect("put");
        c.put("b", b"2", CacheTier::UserContent, &["album"])
            .await
            .expect("put");
        c.put("c", b"3", CacheTier::UserContent, &["other"])
            .await
            .expect("put");
        c.invalidate_tag("album").await;
        c.invalidate_key("c").await;
        for k in ["a", "b", "c"] {
            assert!(matches!(
                c.get(k, CacheTier::UserContent).await,
                CacheResult::Miss
            ));
        }
        let left = fs::read_dir(dir.path().join(format!("v{CURRENT_SCHEMA_VERSION}/user")))
            .expect("dir")
            .count();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn a_refresh_that_started_before_an_invalidation_is_not_written_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let c = cache(dir.path());
        c.put("favs", b"old", CacheTier::UserContent, &["favorites"])
            .await
            .expect("put");

        // Refresh starts, then a favourite is added (invalidating), then the
        // refresh lands with the list it fetched before the add.
        assert!(c.mark_in_flight("favs").await);
        c.invalidate_tag("favorites").await;
        c.put("favs", b"old", CacheTier::UserContent, &["favorites"])
            .await
            .expect("put");
        c.clear_in_flight("favs").await;
        assert!(matches!(
            c.get("favs", CacheTier::UserContent).await,
            CacheResult::Miss
        ));

        // Same for a logout's clear.
        assert!(c.mark_in_flight("favs").await);
        c.clear().await;
        c.put("favs", b"other account", CacheTier::UserContent, &[])
            .await
            .expect("put");
        c.clear_in_flight("favs").await;
        assert!(matches!(
            c.get("favs", CacheTier::UserContent).await,
            CacheResult::Miss
        ));

        // A refresh with no invalidation in between still lands.
        assert!(c.mark_in_flight("favs").await);
        c.put("favs", b"new", CacheTier::UserContent, &["favorites"])
            .await
            .expect("put");
        c.clear_in_flight("favs").await;
        assert!(matches!(
            c.get("favs", CacheTier::UserContent).await,
            CacheResult::Fresh(b) if b == b"new"
        ));
    }

    #[tokio::test]
    async fn clear_empties_every_tier() {
        let dir = tempfile::tempdir().expect("tempdir");
        let c = cache(dir.path());
        c.put("a", b"1", CacheTier::Image, &[]).await.expect("put");
        c.put("b", b"2", CacheTier::StaticMeta, &[])
            .await
            .expect("put");
        c.clear().await;
        assert_eq!(c.stats().await.total_entries, 0);
        assert!(matches!(
            c.get("a", CacheTier::Image).await,
            CacheResult::Miss
        ));
    }

    #[tokio::test]
    async fn a_reopened_cache_still_serves_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        cache(dir.path())
            .put("k", b"kept", CacheTier::StaticMeta, &[])
            .await
            .expect("put");
        match cache(dir.path()).get("k", CacheTier::StaticMeta).await {
            CacheResult::Fresh(b) => assert_eq!(b, b"kept"),
            _ => panic!("expected a hit after reopening"),
        }
    }
}

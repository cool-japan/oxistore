//! [`BlobCache`] — a caching adapter that wraps any [`oxistore_blob::BlobStore`] and keeps
//! recently-fetched blobs in a [`SyncCache`]-backed in-memory LRU.
//!
//! Cache invalidation rules:
//! - `get`: check memory cache first; on miss, fetch from the inner store,
//!   insert into cache, and return.
//! - `put`: evict the cached key (if any), then forward to the inner store.
//! - `delete`: evict the cached key (if any), then forward to the inner store.
//! - `head`, `list`: always forwarded directly (metadata/listing is not cached).
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use oxistore_blob::{BlobError, BlobMeta, BlobStore};

use crate::lru::LruCache;
use crate::sync::SyncCache;

/// Atomic hit/miss counters shared between a [`BlobCache`] and its callers.
#[derive(Debug, Default)]
pub struct BlobCacheStats {
    hits: AtomicU64,
    misses: AtomicU64,
}

impl BlobCacheStats {
    /// Create a new zeroed stats instance.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the number of cache hits recorded.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Return the number of cache misses recorded.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    /// Return the hit rate as a fraction in `[0.0, 1.0]`.
    ///
    /// Returns `0.0` if no accesses have been recorded.
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let h = self.hits();
        let m = self.misses();
        let total = h + m;
        if total == 0 {
            0.0
        } else {
            h as f64 / total as f64
        }
    }

    /// Reset all counters to zero.
    pub fn reset(&self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }
}

/// Type alias for the specific SyncCache used in BlobCache.
type BlobLruSync = SyncCache<String, Bytes, LruCache<String, Bytes>>;

/// A caching wrapper around any [`BlobStore`], keeping recently-fetched blobs
/// in a bounded in-memory LRU cache.
///
/// `get` results are cached; `put` and `delete` both invalidate the relevant
/// cache entry before forwarding to the inner store so stale data is never
/// served.
///
/// # Thread safety
///
/// The internal cache is wrapped in a [`SyncCache`] (Mutex-backed), so
/// `BlobCache` is `Send + Sync` and can be shared across tasks.
pub struct BlobCache<B: BlobStore> {
    inner: B,
    cache: Arc<BlobLruSync>,
    stats: Arc<BlobCacheStats>,
}

impl<B: BlobStore> BlobCache<B> {
    /// Wrap `inner` with an in-memory LRU cache of the given `capacity`
    /// (number of blobs, not bytes).
    pub fn new(inner: B, capacity: usize) -> Self {
        Self {
            inner,
            cache: Arc::new(SyncCache::new(LruCache::new(capacity))),
            stats: Arc::new(BlobCacheStats::new()),
        }
    }

    /// Return a handle to the shared cache statistics.
    ///
    /// The returned [`Arc`] is live — callers see counters update in real time.
    pub fn stats(&self) -> Arc<BlobCacheStats> {
        Arc::clone(&self.stats)
    }
}

impl<B: BlobStore> BlobStore for BlobCache<B> {
    fn get(&self, key: &str) -> impl std::future::Future<Output = Result<Bytes, BlobError>> + Send {
        let cache = Arc::clone(&self.cache);
        let stats = Arc::clone(&self.stats);
        let key_owned = key.to_string();
        async move {
            // Fast path: check in-memory cache first.
            if let Some(cached) = cache.get(&key_owned) {
                stats.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(cached);
            }
            // Slow path: fetch from the inner store.
            stats.misses.fetch_add(1, Ordering::Relaxed);
            let data = self.inner.get(&key_owned).await?;
            cache.put(key_owned, data.clone());
            Ok(data)
        }
    }

    fn put(
        &self,
        key: &str,
        data: Bytes,
    ) -> impl std::future::Future<Output = Result<(), BlobError>> + Send {
        let cache = Arc::clone(&self.cache);
        let key_owned = key.to_string();
        async move {
            // Invalidate cache entry so stale data is not served after an update.
            cache.remove(&key_owned);
            self.inner.put(&key_owned, data).await
        }
    }

    fn delete(&self, key: &str) -> impl std::future::Future<Output = Result<(), BlobError>> + Send {
        let cache = Arc::clone(&self.cache);
        let key_owned = key.to_string();
        async move {
            // Evict from cache before deleting from store.
            cache.remove(&key_owned);
            self.inner.delete(&key_owned).await
        }
    }

    fn head(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<BlobMeta, BlobError>> + Send {
        let key_owned = key.to_string();
        async move { self.inner.head(&key_owned).await }
    }

    fn list(
        &self,
        prefix: &str,
    ) -> impl std::future::Future<Output = Result<Vec<String>, BlobError>> + Send {
        let prefix_owned = prefix.to_string();
        async move { self.inner.list(&prefix_owned).await }
    }
}

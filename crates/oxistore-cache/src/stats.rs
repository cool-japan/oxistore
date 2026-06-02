//! Cache hit/miss statistics.
//!
//! [`CacheStats`] provides atomic counters for cache hits and misses.
//! [`StatsCache`] wraps any `Cache<Vec<u8>, Vec<u8>>` and records access
//! outcomes automatically.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::Cache;

/// Atomic hit/miss counters for a cache.
///
/// All counters use `Relaxed` ordering — precise ordering guarantees across
/// threads are not required for statistics (approximate values are fine).
#[derive(Debug, Default)]
pub struct CacheStats {
    hits: AtomicU64,
    misses: AtomicU64,
}

impl CacheStats {
    /// Create a fresh `CacheStats` with all counters at zero.
    #[must_use]
    pub fn new() -> Self {
        CacheStats::default()
    }

    /// Record a single cache hit.
    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a single cache miss.
    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Return the total number of recorded hits.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Return the total number of recorded misses.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    /// Return the hit rate as a fraction in `[0.0, 1.0]`.
    ///
    /// Returns `0.0` if no accesses have been recorded yet.
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

/// A cache wrapper that records hits and misses via [`CacheStats`].
///
/// On each `get` call:
/// - If the inner cache returns `Some(v)` → hit recorded.
/// - If the inner cache returns `None`    → miss recorded.
///
/// # Type parameters
///
/// - `C`: inner `Cache<Vec<u8>, Vec<u8>>` implementation.
pub struct StatsCache<C> {
    inner: C,
    stats: Arc<CacheStats>,
}

impl<C> StatsCache<C>
where
    C: Cache<Vec<u8>, Vec<u8>>,
{
    /// Wrap `inner` with a freshly created `CacheStats`.
    pub fn new(inner: C) -> Self {
        StatsCache {
            inner,
            stats: Arc::new(CacheStats::new()),
        }
    }

    /// Wrap `inner` with a shared `CacheStats` (useful for sharing across wrappers).
    pub fn with_stats(inner: C, stats: Arc<CacheStats>) -> Self {
        StatsCache { inner, stats }
    }

    /// Return a reference to the underlying stats.
    #[must_use]
    pub fn stats(&self) -> &Arc<CacheStats> {
        &self.stats
    }
}

impl<C> Cache<Vec<u8>, Vec<u8>> for StatsCache<C>
where
    C: Cache<Vec<u8>, Vec<u8>>,
{
    fn get(&mut self, key: &Vec<u8>) -> Option<&Vec<u8>> {
        let result = self.inner.get(key);
        if result.is_some() {
            self.stats.record_hit();
        } else {
            self.stats.record_miss();
        }
        result
    }

    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Option<Vec<u8>> {
        self.inner.put(key, value)
    }

    fn put_with_ttl(
        &mut self,
        key: Vec<u8>,
        value: Vec<u8>,
        ttl: std::time::Duration,
    ) -> Option<Vec<u8>> {
        self.inner.put_with_ttl(key, value, ttl)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn cap(&self) -> usize {
        self.inner.cap()
    }

    fn remove(&mut self, key: &Vec<u8>) -> Option<Vec<u8>> {
        self.inner.remove(key)
    }

    fn clear(&mut self) {
        self.inner.clear();
    }

    fn peek(&self, key: &Vec<u8>) -> Option<&Vec<u8>> {
        self.inner.peek(key)
    }

    fn contains_key(&self, key: &Vec<u8>) -> bool {
        self.inner.contains_key(key)
    }

    fn resize(&mut self, new_cap: usize) {
        self.inner.resize(new_cap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LruCache;

    #[test]
    fn stats_initial_state() {
        let stats = CacheStats::new();
        assert_eq!(stats.hits(), 0);
        assert_eq!(stats.misses(), 0);
        assert!((stats.hit_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_record_hit_miss() {
        let stats = CacheStats::new();
        stats.record_hit();
        stats.record_hit();
        stats.record_miss();
        assert_eq!(stats.hits(), 2);
        assert_eq!(stats.misses(), 1);
        let rate = stats.hit_rate();
        assert!((rate - 2.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn stats_reset() {
        let stats = CacheStats::new();
        stats.record_hit();
        stats.record_miss();
        stats.reset();
        assert_eq!(stats.hits(), 0);
        assert_eq!(stats.misses(), 0);
    }

    #[test]
    fn stats_cache_hit_and_miss() {
        let inner = LruCache::<Vec<u8>, Vec<u8>>::new(4);
        let mut cache = StatsCache::new(inner);

        cache.put(b"hello".to_vec(), b"world".to_vec());

        // Hit
        let v = cache.get(&b"hello".to_vec());
        assert_eq!(v, Some(&b"world".to_vec()));
        assert_eq!(cache.stats().hits(), 1);
        assert_eq!(cache.stats().misses(), 0);

        // Miss
        let v = cache.get(&b"missing".to_vec());
        assert!(v.is_none());
        assert_eq!(cache.stats().hits(), 1);
        assert_eq!(cache.stats().misses(), 1);

        let rate = cache.stats().hit_rate();
        assert!((rate - 0.5).abs() < 1e-10);
    }

    #[test]
    fn stats_cache_delegates_put_remove_clear() {
        let inner = LruCache::<Vec<u8>, Vec<u8>>::new(4);
        let mut cache = StatsCache::new(inner);

        cache.put(b"k".to_vec(), b"v".to_vec());
        assert_eq!(cache.len(), 1);

        cache.remove(&b"k".to_vec());
        assert_eq!(cache.len(), 0);

        cache.put(b"a".to_vec(), b"1".to_vec());
        cache.put(b"b".to_vec(), b"2".to_vec());
        cache.clear();
        assert!(cache.is_empty());
    }
}

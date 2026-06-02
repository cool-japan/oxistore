//! Cache builder — ergonomic constructor for all cache policies.
//!
//! [`CacheBuilder`] lets callers configure a cache through a fluent API and
//! then construct the desired implementation.  Optional fields (`max_bytes`,
//! `n_shards`) are used by the wrapper types but are not required for basic
//! policy caches.

use crate::bounded::BoundedCache;
use crate::sharded::ShardedCache;
use crate::{ArcCache, LfuCache, LruCache, WTinyLfuCache};

/// The eviction policy to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    /// Least-Recently-Used — classic recency-based eviction.
    Lru,
    /// Adaptive Replacement Cache — balances recency and frequency.
    Arc,
    /// Least-Frequently-Used — O(1) frequency-based eviction.
    Lfu,
    /// Window TinyLFU — near-optimal admission policy with CMS frequency estimation.
    WTinyLfu,
}

/// Builder for cache instances.
///
/// # Examples
///
/// ```rust
/// use oxistore_cache::builder::{CacheBuilder, CachePolicy};
///
/// let lru = CacheBuilder::new(128)
///     .policy(CachePolicy::Lru)
///     .build_lru();
///
/// let arc = CacheBuilder::new(256)
///     .policy(CachePolicy::Arc)
///     .build_arc();
/// ```
#[derive(Debug, Clone)]
pub struct CacheBuilder {
    /// Number of cache entries (item count, not bytes).
    capacity: usize,
    /// Eviction policy selection (informational; concrete build methods ignore it).
    policy: CachePolicy,
    /// Optional byte-budget cap for `BoundedCache`.
    max_bytes: Option<usize>,
    /// Optional shard count for `ShardedCache` (must be power of 2).
    n_shards: Option<usize>,
}

impl CacheBuilder {
    /// Create a builder with the given entry-count capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        CacheBuilder {
            capacity,
            policy: CachePolicy::Lru,
            max_bytes: None,
            n_shards: None,
        }
    }

    /// Set the eviction policy.
    #[must_use]
    pub fn policy(mut self, p: CachePolicy) -> Self {
        self.policy = p;
        self
    }

    /// Set a byte-budget limit (used by [`CacheBuilder::build_bounded_lru`]).
    #[must_use]
    pub fn max_bytes(mut self, b: usize) -> Self {
        self.max_bytes = Some(b);
        self
    }

    /// Set the number of shards (used by [`CacheBuilder::build_sharded`]).
    ///
    /// Must be a power of two.
    #[must_use]
    pub fn n_shards(mut self, n: usize) -> Self {
        self.n_shards = Some(n);
        self
    }

    /// Build a [`LruCache`] with the configured capacity.
    #[must_use]
    pub fn build_lru(self) -> LruCache<Vec<u8>, Vec<u8>> {
        LruCache::new(self.capacity)
    }

    /// Build an [`ArcCache`] with the configured capacity.
    #[must_use]
    pub fn build_arc(self) -> ArcCache<Vec<u8>, Vec<u8>> {
        ArcCache::new(self.capacity)
    }

    /// Build an [`LfuCache`] with the configured capacity.
    #[must_use]
    pub fn build_lfu(self) -> LfuCache<Vec<u8>, Vec<u8>> {
        LfuCache::new(self.capacity)
    }

    /// Build a [`WTinyLfuCache`] with the configured capacity.
    #[must_use]
    pub fn build_wtinylfu(self) -> WTinyLfuCache<Vec<u8>, Vec<u8>> {
        WTinyLfuCache::new(self.capacity)
    }

    /// Build a [`BoundedCache`] wrapping an LRU inner cache.
    ///
    /// Uses `max_bytes` if set; otherwise defaults to `capacity * 64` (a rough
    /// 64-byte average per entry).
    #[must_use]
    pub fn build_bounded_lru(self) -> BoundedCache<LruCache<Vec<u8>, Vec<u8>>> {
        let max_bytes = self.max_bytes.unwrap_or(self.capacity * 64);
        BoundedCache::new(LruCache::new(self.capacity), max_bytes)
    }

    /// Build a [`ShardedCache`] with LRU shards.
    ///
    /// Uses `n_shards` if set; otherwise defaults to 8.
    /// The capacity is split evenly across shards (`capacity / n_shards`).
    ///
    /// # Panics
    ///
    /// Panics if the resolved `n_shards` is not a power of two or is zero.
    #[must_use]
    pub fn build_sharded(self) -> ShardedCache {
        let n = self.n_shards.unwrap_or(8);
        let shard_cap = (self.capacity / n).max(1);
        ShardedCache::new(n, shard_cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cache;

    #[test]
    fn builder_lru() {
        let mut cache = CacheBuilder::new(4).policy(CachePolicy::Lru).build_lru();
        cache.put(b"k".to_vec(), b"v".to_vec());
        assert_eq!(cache.get(&b"k".to_vec()), Some(&b"v".to_vec()));
        assert_eq!(cache.cap(), 4);
    }

    #[test]
    fn builder_arc() {
        let mut cache = CacheBuilder::new(4).policy(CachePolicy::Arc).build_arc();
        cache.put(b"k".to_vec(), b"v".to_vec());
        assert_eq!(cache.get(&b"k".to_vec()), Some(&b"v".to_vec()));
        assert_eq!(cache.cap(), 4);
    }

    #[test]
    fn builder_lfu() {
        let mut cache = CacheBuilder::new(4).policy(CachePolicy::Lfu).build_lfu();
        cache.put(b"k".to_vec(), b"v".to_vec());
        assert_eq!(cache.get(&b"k".to_vec()), Some(&b"v".to_vec()));
        assert_eq!(cache.cap(), 4);
    }

    #[test]
    fn builder_wtinylfu() {
        let mut cache = CacheBuilder::new(10)
            .policy(CachePolicy::WTinyLfu)
            .build_wtinylfu();
        cache.put(b"k".to_vec(), b"v".to_vec());
        assert_eq!(cache.cap(), 10);
    }

    #[test]
    fn builder_bounded_default_budget() {
        let cache = CacheBuilder::new(8).build_bounded_lru();
        assert_eq!(cache.max_bytes(), 8 * 64);
    }

    #[test]
    fn builder_bounded_explicit_budget() {
        let mut cache = CacheBuilder::new(100).max_bytes(50).build_bounded_lru();
        assert_eq!(cache.max_bytes(), 50);
        // Insert entries that together are at most 50 bytes.
        cache.put(b"key1".to_vec(), b"val1".to_vec()); // 8 bytes
        assert!(cache.current_bytes() <= 50);
    }

    #[test]
    fn builder_sharded_default() {
        let cache = CacheBuilder::new(64).build_sharded();
        assert_eq!(cache.n_shards(), 8);
    }

    #[test]
    fn builder_sharded_custom_shards() {
        let cache = CacheBuilder::new(32).n_shards(4).build_sharded();
        assert_eq!(cache.n_shards(), 4);
        assert_eq!(cache.shard_cap(), 8); // 32 / 4
    }

    #[test]
    fn builder_each_policy_usable() {
        // Smoke-test: each policy type can be constructed and used.
        let mut lru = CacheBuilder::new(4).build_lru();
        lru.put(b"a".to_vec(), b"1".to_vec());

        let mut arc = CacheBuilder::new(4).build_arc();
        arc.put(b"a".to_vec(), b"1".to_vec());

        let mut lfu = CacheBuilder::new(4).build_lfu();
        lfu.put(b"a".to_vec(), b"1".to_vec());

        let mut wtlfu = CacheBuilder::new(10).build_wtinylfu();
        wtlfu.put(b"a".to_vec(), b"1".to_vec());

        let mut bounded = CacheBuilder::new(4).max_bytes(200).build_bounded_lru();
        bounded.put(b"a".to_vec(), b"1".to_vec());

        let sharded = CacheBuilder::new(16).n_shards(4).build_sharded();
        sharded.put(b"a".to_vec(), b"1".to_vec());
        assert_eq!(sharded.get(b"a"), Some(b"1".to_vec()));
    }
}

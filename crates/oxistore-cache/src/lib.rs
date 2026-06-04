#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore-cache` — Pure-Rust LRU, ARC, LFU, and W-TinyLFU eviction primitives.
//!
//! This crate provides four cache implementations behind a unified [`Cache`]
//! trait:
//!
//! - [`LruCache`] — classic Least-Recently-Used cache backed by a
//!   `hashlink::LinkedHashMap`.  O(1) amortised operations.
//!
//! - [`ArcCache`] — Adaptive Replacement Cache (Megiddo & Modha, FAST'03),
//!   which balances recency and frequency by maintaining four lists (T1, T2,
//!   B1, B2) and an adaptive target `p`.  ARC is scan-resistant.
//!
//! - [`LfuCache`] — Least-Frequently-Used cache with O(1) operations using
//!   the Shah, Mitra & Matani (2010) algorithm.
//!
//! - [`WTinyLfuCache`] — Window TinyLFU with Count-Min Sketch frequency
//!   estimation and a doorkeeper bloom filter, providing near-optimal hit rates
//!   on skewed (Zipfian) workloads.
//!
//! All cache implementations support per-entry TTL (time-to-live) via the
//! [`Cache::put_with_ttl`] method.  Expiry is checked lazily on access.
//!
//! # Example
//!
//! ```rust
//! use oxistore_cache::{LruCache, ArcCache, LfuCache, WTinyLfuCache, Cache};
//!
//! let mut lru = LruCache::new(3);
//! lru.put(1u32, "a");
//! lru.put(2u32, "b");
//! lru.put(3u32, "c");
//! lru.put(4u32, "d"); // evicts 1 (LRU)
//! assert!(lru.get(&1u32).is_none());
//! assert_eq!(lru.get(&2u32), Some(&"b"));
//!
//! let mut arc = ArcCache::new(3);
//! arc.put(1u32, "a");
//! arc.put(2u32, "b");
//! arc.put(3u32, "c");
//!
//! let mut lfu = LfuCache::new(3);
//! lfu.put(1u32, "a");
//! lfu.put(2u32, "b");
//! lfu.put(3u32, "c");
//!
//! let mut wtlfu = WTinyLfuCache::new(3);
//! wtlfu.put(1u32, "a");
//! wtlfu.put(2u32, "b");
//! wtlfu.put(3u32, "c");
//! ```

/// Adaptive Replacement Cache (ARC) — balances recency and frequency.
pub mod arc;
/// Bounded-memory cache wrapper — enforces a hard byte-budget cap.
pub mod bounded;
/// Cache builder — ergonomic constructor for all cache policies.
pub mod builder;
/// Least-Frequently-Used (LFU) cache — O(1) frequency-based eviction.
pub mod lfu;
/// Least-Recently-Used (LRU) cache — evicts the least recently accessed entry.
pub mod lru;
/// Sharded concurrent cache — N LRU shards behind a Mutex for low contention.
pub mod sharded;
/// Count-Min Sketch and Doorkeeper bloom filter for W-TinyLFU.
pub mod sketch;
/// Cache hit/miss statistics tracking.
pub mod stats;
/// Thread-safe cache wrapper — wraps any `Cache` impl behind a `Mutex`.
pub mod sync;
/// Window TinyLFU — state-of-the-art admission policy with CMS frequency estimator.
pub mod tinylfu;
/// Write-through and write-back cache adapters.
pub mod write_adapter;

/// Caching wrapper for `BlobStore` backends (see `oxistore_blob::BlobStore`).
#[cfg(feature = "blob")]
pub mod blob_cache;

/// Caching adapter for hot Parquet row groups from `oxistore-columnar`.
#[cfg(feature = "columnar")]
pub mod columnar_cache;

/// SQL query-result and prepared-plan caches backed by `oxisql-core` types.
#[cfg(feature = "sql")]
pub mod sql_cache;

pub use arc::ArcCache;
pub use bounded::BoundedCache;
pub use builder::{CacheBuilder, CachePolicy};
pub use lfu::LfuCache;
pub use lru::LruCache;
pub use sharded::ShardedCache;
pub use stats::{CacheStats, StatsCache};
pub use sync::SyncCache;
pub use tinylfu::WTinyLfuCache;
pub use write_adapter::{CacheableKvStore, WriteBackCache, WriteThroughCache};

#[cfg(feature = "blob")]
pub use blob_cache::BlobCache;

#[cfg(feature = "columnar")]
pub use columnar_cache::ColumnarRowGroupCache;

#[cfg(feature = "sql")]
pub use sql_cache::{CachedQueryRunner, SqlPlanCache, SqlQueryCache};

/// A cache entry that optionally expires at a given instant.
///
/// Used internally by all cache implementations to support per-entry TTL.
/// Callers interact with the value type `V` via the [`Cache`] trait methods;
/// `CacheEntry` is exposed for advanced use cases (e.g. custom wrappers).
pub struct CacheEntry<V> {
    /// The stored value.
    pub value: V,
    /// Optional expiry time.  `None` means the entry never expires.
    pub expires_at: Option<std::time::Instant>,
}

impl<V> CacheEntry<V> {
    /// Create a non-expiring entry.
    #[must_use]
    pub fn new(value: V) -> Self {
        CacheEntry {
            value,
            expires_at: None,
        }
    }

    /// Create an entry that expires after `ttl` from now.
    #[must_use]
    pub fn with_ttl(value: V, ttl: std::time::Duration) -> Self {
        CacheEntry {
            value,
            expires_at: Some(std::time::Instant::now() + ttl),
        }
    }

    /// Return `true` if this entry has expired (i.e. its deadline has passed).
    #[must_use]
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(deadline) => std::time::Instant::now() >= deadline,
            None => false,
        }
    }
}

/// Unified interface for key-value caches with bounded capacity.
///
/// All four implementations ([`LruCache`], [`ArcCache`], [`LfuCache`],
/// [`WTinyLfuCache`]) implement this trait, allowing callers to write generic
/// code against the cache interface.
pub trait Cache<K, V> {
    /// Look up `key`, returning a reference to the value if present.
    ///
    /// Implementations update internal bookkeeping (e.g. promoting the entry
    /// to MRU or incrementing frequency counts) as a side effect.
    ///
    /// If the entry exists but has expired (TTL), it is removed and `None` is
    /// returned without updating recency or frequency.
    fn get(&mut self, key: &K) -> Option<&V>;

    /// Insert or update `key` -> `value` without a TTL.
    ///
    /// If inserting a new key would exceed the cache's capacity, the
    /// implementation evicts one entry (per its policy) and returns the
    /// evicted value.  On a key update, the old value is not returned.
    fn put(&mut self, key: K, value: V) -> Option<V>;

    /// Insert or update `key` -> `value` with a time-to-live.
    ///
    /// After `ttl` has elapsed, any access to `key` will treat it as a miss
    /// and remove the entry lazily.
    ///
    /// Concrete implementations in this crate override this to actually store
    /// the expiry.  External impls that don't need TTL may rely on the default
    /// which falls back to a plain `put` (no expiry).
    fn put_with_ttl(&mut self, key: K, value: V, ttl: std::time::Duration) -> Option<V> {
        let _ = ttl;
        self.put(key, value)
    }

    /// Return the number of live entries currently in the cache.
    fn len(&self) -> usize;

    /// Return the maximum number of entries the cache can hold.
    fn cap(&self) -> usize;

    /// Return `true` if the cache holds no entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove the entry associated with `key`, returning its value if present.
    ///
    /// Unlike eviction, this is an explicit removal requested by the caller.
    fn remove(&mut self, key: &K) -> Option<V>;

    /// Remove all entries from the cache.
    fn clear(&mut self);

    /// Look up `key` without updating access metadata (no promotion).
    ///
    /// If the entry has expired (TTL), it is removed and `None` is returned.
    /// Returns `None` if the key is not present or has expired.
    fn peek(&self, key: &K) -> Option<&V>;

    /// Return `true` if `key` is present in the cache (without promotion).
    ///
    /// Expired entries are treated as absent.
    fn contains_key(&self, key: &K) -> bool;

    /// Dynamically resize the cache capacity.
    ///
    /// If `new_cap` is smaller than the current length, excess entries are
    /// evicted according to the cache's eviction policy.
    fn resize(&mut self, new_cap: usize);

    /// Return `&V` for `key`, inserting `default()` if the key is absent.
    ///
    /// The closure `default` is called at most once.  If the key is already
    /// present the closure is never invoked.
    ///
    /// Implementations may override this for efficiency (e.g. to avoid a
    /// second hash lookup).  The default implementation uses [`Cache::peek`]
    /// after ensuring the key exists.
    fn get_or_insert(&mut self, key: K, default: impl FnOnce() -> V) -> &V
    where
        K: Clone,
    {
        if !self.contains_key(&key) {
            let v = default();
            self.put(key.clone(), v);
        }
        // peek is &self and won't panic since we just ensured the key exists.
        self.peek(&key).expect("key was just inserted")
    }

    /// Return all live (non-expired) values currently stored in the cache.
    ///
    /// The default implementation returns an empty `Vec`.  Concrete
    /// implementations override this to return actual values.
    fn values(&self) -> Vec<&V> {
        Vec::new()
    }

    /// Pre-populate the cache with `(key, value)` pairs from an iterator.
    ///
    /// Each pair is inserted via [`Cache::put`], respecting the cache's
    /// eviction policy.  Pairs that exceed capacity cause earlier entries to
    /// be evicted according to the policy.
    fn warm(&mut self, iter: impl IntoIterator<Item = (K, V)>) {
        for (k, v) in iter {
            self.put(k, v);
        }
    }
}

// ── Async helper ─────────────────────────────────────────────────────────────

/// Look up `key` in `cache`, returning the cached value if present.
///
/// If the key is absent, the async closure `loader` is awaited to produce
/// a value, which is then inserted into the cache and returned.
///
/// The closure is invoked **at most once** per call.  If the key is already
/// present it is never called.
///
/// # Example
///
/// ```rust
/// use oxistore_cache::{LruCache, Cache, get_or_insert_async};
/// use std::sync::Mutex;
///
/// # async fn example() {
/// let cache = Mutex::new(LruCache::<u32, String>::new(4));
/// let val = get_or_insert_async(
///     &cache,
///     42u32,
///     || async { "computed".to_string() },
/// ).await;
/// assert_eq!(val, "computed");
/// # }
/// ```
pub async fn get_or_insert_async<K, V, C, F, Fut>(
    cache: &std::sync::Mutex<C>,
    key: K,
    loader: F,
) -> V
where
    K: Eq + std::hash::Hash + Clone,
    V: Clone,
    C: Cache<K, V>,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = V>,
{
    // Fast path: key already in cache.
    {
        let mut guard = cache.lock().expect("cache mutex poisoned");
        if let Some(v) = guard.get(&key) {
            return v.clone();
        }
    }
    // Slow path: key absent — await the loader outside the lock.
    let value = loader().await;
    {
        let mut guard = cache.lock().expect("cache mutex poisoned");
        // Double-check in case another task inserted the key while we waited.
        if let Some(existing) = guard.peek(&key) {
            return existing.clone();
        }
        guard.put(key, value.clone());
    }
    value
}

/// Look up `key` in a `tokio::sync::Mutex`-wrapped cache asynchronously.
///
/// Identical semantics to [`get_or_insert_async`] but uses an async
/// `tokio::sync::Mutex` instead of `std::sync::Mutex`.
///
/// This variant is suitable when the cache is shared across `tokio` tasks
/// that run on a multi-threaded executor and cannot use a synchronous lock.
///
/// Requires the `async-helpers` feature flag.
#[cfg(feature = "async-helpers")]
pub async fn get_or_insert_async_tokio<K, V, C, F, Fut>(
    cache: &tokio::sync::Mutex<C>,
    key: K,
    loader: F,
) -> V
where
    K: Eq + std::hash::Hash + Clone,
    V: Clone,
    C: Cache<K, V>,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = V>,
{
    // Fast path: key already in cache (async lock, no blocking).
    {
        let mut guard = cache.lock().await;
        if let Some(v) = guard.get(&key) {
            return v.clone();
        }
    }
    // Slow path: await the loader without holding the lock.
    let value = loader().await;
    {
        let mut guard = cache.lock().await;
        if let Some(existing) = guard.peek(&key) {
            return existing.clone();
        }
        guard.put(key, value.clone());
    }
    value
}

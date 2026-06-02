//! Sharded concurrent cache.
//!
//! [`ShardedCache`] wraps `N` independent [`LruCache`] shards behind a
//! `Mutex` each, reducing lock contention under parallel workloads.
//! `N` must be a power of two so that routing can use a fast bitmask instead
//! of a modulo operation.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use crate::{Cache, LruCache};

/// A concurrent cache backed by N power-of-two LRU shards.
///
/// Keys are routed to shards via `hash(key) & (n_shards - 1)`.  Each shard
/// is independently protected by a `Mutex<LruCache<Vec<u8>, Vec<u8>>>`.
///
/// # Panics
///
/// `new` panics if `n_shards` is not a power of two, or if `n_shards` is 0.
pub struct ShardedCache {
    shards: Vec<Mutex<LruCache<Vec<u8>, Vec<u8>>>>,
    /// Bitmask = n_shards - 1 (valid because n_shards is power of 2).
    mask: usize,
    /// Capacity per shard.
    shard_cap: usize,
}

impl ShardedCache {
    /// Create a new sharded cache.
    ///
    /// - `n_shards`: number of shards — must be a power of two.
    /// - `shard_cap`: capacity per shard (entry count).
    ///
    /// # Panics
    ///
    /// Panics if `n_shards == 0` or `!n_shards.is_power_of_two()`.
    pub fn new(n_shards: usize, shard_cap: usize) -> Self {
        assert!(
            n_shards > 0 && n_shards.is_power_of_two(),
            "n_shards must be a positive power of two, got {n_shards}"
        );
        let shards = (0..n_shards)
            .map(|_| Mutex::new(LruCache::new(shard_cap)))
            .collect();
        ShardedCache {
            shards,
            mask: n_shards - 1,
            shard_cap,
        }
    }

    /// Return the number of shards.
    #[must_use]
    pub fn n_shards(&self) -> usize {
        self.shards.len()
    }

    /// Capacity per shard.
    #[must_use]
    pub fn shard_cap(&self) -> usize {
        self.shard_cap
    }

    /// Hash a key to its shard index.
    fn shard_index(&self, key: &[u8]) -> usize {
        let mut h = DefaultHasher::new();
        key.hash(&mut h);
        (h.finish() as usize) & self.mask
    }

    /// Acquire the shard mutex for `key`.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned.
    fn shard(&self, key: &[u8]) -> std::sync::MutexGuard<'_, LruCache<Vec<u8>, Vec<u8>>> {
        let idx = self.shard_index(key);
        self.shards[idx].lock().expect("shard mutex poisoned")
    }

    /// Look up `key`, returning a cloned copy of the value if present.
    ///
    /// Returns `None` if the key is absent or expired.
    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.shard(key).get(&key.to_vec()).cloned()
    }

    /// Insert or update `key` -> `value`.
    pub fn put(&self, key: Vec<u8>, value: Vec<u8>) {
        self.shard(&key).put(key, value);
    }

    /// Remove `key`, returning its value if present.
    pub fn remove(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.shard(key).remove(&key.to_vec())
    }

    /// Return `true` if `key` is present and not expired.
    pub fn contains(&self, key: &[u8]) -> bool {
        self.shard(key).contains_key(&key.to_vec())
    }

    /// Return the total number of live entries across all shards.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.lock().expect("shard mutex poisoned").len())
            .sum()
    }

    /// Return `true` if no entries are in any shard.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all shards.
    pub fn clear(&self) {
        for shard in &self.shards {
            shard.lock().expect("shard mutex poisoned").clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    #[should_panic]
    fn sharded_panics_on_non_power_of_two() {
        let _ = ShardedCache::new(3, 10);
    }

    #[test]
    fn sharded_basic_put_get() {
        let cache = ShardedCache::new(4, 16);
        cache.put(b"hello".to_vec(), b"world".to_vec());
        assert_eq!(cache.get(b"hello"), Some(b"world".to_vec()));
        assert!(cache.get(b"missing").is_none());
    }

    #[test]
    fn sharded_remove() {
        let cache = ShardedCache::new(4, 16);
        cache.put(b"k".to_vec(), b"v".to_vec());
        assert!(cache.contains(b"k"));
        let v = cache.remove(b"k");
        assert_eq!(v, Some(b"v".to_vec()));
        assert!(!cache.contains(b"k"));
    }

    #[test]
    fn sharded_len_and_clear() {
        let cache = ShardedCache::new(4, 16);
        cache.put(b"a".to_vec(), b"1".to_vec());
        cache.put(b"b".to_vec(), b"2".to_vec());
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn sharded_concurrent_puts() {
        let cache = Arc::new(ShardedCache::new(8, 256));
        let n_threads = 8;
        let keys_per_thread = 32;

        let handles: Vec<_> = (0..n_threads)
            .map(|t| {
                let cache = Arc::clone(&cache);
                thread::spawn(move || {
                    for i in 0..keys_per_thread {
                        let key = format!("thread{t}_key{i}").into_bytes();
                        let val = format!("val{i}").into_bytes();
                        cache.put(key, val);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        // All values should be retrievable.
        for t in 0..n_threads {
            for i in 0..keys_per_thread {
                let key = format!("thread{t}_key{i}").into_bytes();
                let expected = format!("val{i}").into_bytes();
                assert_eq!(
                    cache.get(&key),
                    Some(expected),
                    "missing key thread{t}_key{i}"
                );
            }
        }
    }
}

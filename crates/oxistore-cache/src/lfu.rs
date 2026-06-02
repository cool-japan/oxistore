/// O(1) Least-Frequently-Used (LFU) cache.
///
/// Implements the constant-time LFU algorithm from:
///   Shah, K., Mitra, A. & Matani, D. (2010). An O(1) algorithm for
///   implementing the LFU cache eviction scheme. Technical Report.
///
/// ## Data structures
///
/// - `key_to_entry`: `HashMap<K, (freq, CacheEntry<V>)>` — maps each key to
///   its current frequency count and cached value.
/// - `freq_to_keys`: `HashMap<u64, LinkedHashMap<K, ()>>` — maps each
///   frequency to a FIFO-ordered set of keys at that frequency.  Within a
///   frequency bucket, the *front* is the oldest (to be evicted first).
/// - `min_freq`: the smallest frequency present in the cache; used to locate
///   the eviction candidate in O(1).
///
/// ## Complexity
///
/// All operations (`get`, `put`, eviction) run in O(1) amortized time.
use std::collections::HashMap;
use std::hash::Hash;

use hashlink::LinkedHashMap;

use crate::{Cache, CacheEntry};

/// An LFU cache with a fixed capacity and optional per-entry TTL.
///
/// Entries with equal frequency are evicted in FIFO order (the entry inserted
/// earlier at that frequency is evicted first).
///
/// # Type parameters
///
/// - `K`: key type — must be `Eq + Hash + Clone`.
/// - `V`: value type.
pub struct LfuCache<K, V> {
    cap: usize,
    min_freq: u64,
    /// key -> (frequency, entry)
    key_to_entry: HashMap<K, (u64, CacheEntry<V>), ahash::RandomState>,
    /// frequency -> FIFO-ordered key set (front = oldest = next to evict)
    freq_to_keys: HashMap<u64, LinkedHashMap<K, (), ahash::RandomState>, ahash::RandomState>,
}

impl<K, V> LfuCache<K, V>
where
    K: Eq + Hash + Clone,
{
    /// Create a new `LfuCache` with the given capacity.
    ///
    /// A capacity of `0` is valid; every insert will immediately be evicted on
    /// the next insert.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        LfuCache {
            cap,
            min_freq: 0,
            key_to_entry: HashMap::with_hasher(ahash::RandomState::default()),
            freq_to_keys: HashMap::with_hasher(ahash::RandomState::default()),
        }
    }

    /// Increment the frequency of an existing key and update bookkeeping.
    ///
    /// This is called both on `get` (after a hit) and on `put` (when updating
    /// an existing key).  The caller must ensure `key` is present in
    /// `key_to_entry`.
    fn increment_freq(&mut self, key: &K) {
        let (freq, _entry) = match self.key_to_entry.get_mut(key) {
            Some(pair) => pair,
            None => return,
        };
        let old_freq = *freq;
        let new_freq = old_freq + 1;
        *freq = new_freq;

        // Remove from old frequency bucket.
        if let Some(bucket) = self.freq_to_keys.get_mut(&old_freq) {
            bucket.remove(key);
            if bucket.is_empty() {
                self.freq_to_keys.remove(&old_freq);
                // If we just emptied the min-freq bucket, advance min_freq.
                if old_freq == self.min_freq {
                    self.min_freq = new_freq;
                }
            }
        }

        // Insert into new frequency bucket at the back (MRU within frequency).
        self.freq_to_keys
            .entry(new_freq)
            .or_insert_with(|| LinkedHashMap::with_hasher(ahash::RandomState::default()))
            .insert(key.clone(), ());
    }

    /// Evict the entry with the lowest frequency (FIFO within that frequency).
    fn evict(&mut self) -> Option<V> {
        let bucket = self.freq_to_keys.get_mut(&self.min_freq)?;
        // pop_front gives us the oldest key at this frequency.
        let (evict_key, ()) = bucket.pop_front()?;
        if bucket.is_empty() {
            self.freq_to_keys.remove(&self.min_freq);
        }
        let (_freq, entry) = self.key_to_entry.remove(&evict_key)?;
        Some(entry.value)
    }

    /// Internal put: inserts or updates a key with a pre-built `CacheEntry`.
    fn insert_entry(&mut self, key: K, entry: CacheEntry<V>) -> Option<V> {
        if self.cap == 0 {
            return Some(entry.value);
        }

        // Key already present: update value + TTL and increment frequency.
        if self.key_to_entry.contains_key(&key) {
            let pair = self.key_to_entry.get_mut(&key).expect("confirmed present");
            pair.1 = entry;
            self.increment_freq(&key);
            return None;
        }

        // New key: evict if at capacity.
        let evicted = if self.key_to_entry.len() >= self.cap {
            self.evict()
        } else {
            None
        };

        // Insert with frequency = 1.
        self.key_to_entry.insert(key.clone(), (1, entry));
        self.freq_to_keys
            .entry(1)
            .or_insert_with(|| LinkedHashMap::with_hasher(ahash::RandomState::default()))
            .insert(key, ());
        self.min_freq = 1;

        evicted
    }

    /// Return the value for `key`, incrementing its frequency.
    ///
    /// If the entry has expired (TTL), it is removed and `None` is returned
    /// without updating frequency or recency.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        // Check expiry first.
        let expired = self
            .key_to_entry
            .get(key)
            .map(|(_, e)| e.is_expired())
            .unwrap_or(false);

        if expired {
            // Remove from key_to_entry and freq_to_keys.
            if let Some((freq, _entry)) = self.key_to_entry.remove(key) {
                if let Some(bucket) = self.freq_to_keys.get_mut(&freq) {
                    bucket.remove(key);
                    if bucket.is_empty() {
                        self.freq_to_keys.remove(&freq);
                    }
                }
            }
            return None;
        }

        if !self.key_to_entry.contains_key(key) {
            return None;
        }

        self.increment_freq(key);
        self.key_to_entry.get(key).map(|(_, e)| &e.value)
    }

    /// Insert or update `key` -> `value` (no TTL).
    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        self.insert_entry(key, CacheEntry::new(value))
    }

    /// Insert or update `key` -> `value` with a TTL.
    pub fn put_with_ttl(&mut self, key: K, value: V, ttl: std::time::Duration) -> Option<V> {
        self.insert_entry(key, CacheEntry::with_ttl(value, ttl))
    }

    /// Number of entries currently in the cache.
    #[must_use]
    pub fn len(&self) -> usize {
        self.key_to_entry.len()
    }

    /// Return `true` if the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.key_to_entry.is_empty()
    }

    /// Cache capacity.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Read a value without updating frequency or recency.
    ///
    /// Returns `None` if absent or expired.
    #[must_use]
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.key_to_entry.get(key).and_then(
            |(_, e)| {
                if e.is_expired() {
                    None
                } else {
                    Some(&e.value)
                }
            },
        )
    }

    /// Return `true` if `key` is present and not expired.
    #[must_use]
    pub fn contains_key(&self, key: &K) -> bool {
        self.key_to_entry
            .get(key)
            .map(|(_, e)| !e.is_expired())
            .unwrap_or(false)
    }

    /// Remove the entry for `key`, returning its value.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        if let Some((freq, entry)) = self.key_to_entry.remove(key) {
            if let Some(bucket) = self.freq_to_keys.get_mut(&freq) {
                bucket.remove(key);
                if bucket.is_empty() {
                    self.freq_to_keys.remove(&freq);
                }
            }
            Some(entry.value)
        } else {
            None
        }
    }

    /// Remove all entries.
    pub fn clear(&mut self) {
        self.key_to_entry.clear();
        self.freq_to_keys.clear();
        self.min_freq = 0;
    }

    /// Dynamically resize the cache capacity.
    ///
    /// If `new_cap < current len`, entries at `min_freq` are evicted first
    /// (FIFO within that frequency), then the next lowest frequency, etc.
    pub fn resize(&mut self, new_cap: usize) {
        self.cap = new_cap;
        while self.cap > 0 && self.key_to_entry.len() > self.cap {
            self.evict();
        }
    }
}

impl<K, V> Cache<K, V> for LfuCache<K, V>
where
    K: Eq + Hash + Clone,
{
    fn get(&mut self, key: &K) -> Option<&V> {
        LfuCache::get(self, key)
    }

    fn put(&mut self, key: K, value: V) -> Option<V> {
        LfuCache::put(self, key, value)
    }

    fn put_with_ttl(&mut self, key: K, value: V, ttl: std::time::Duration) -> Option<V> {
        LfuCache::put_with_ttl(self, key, value, ttl)
    }

    fn len(&self) -> usize {
        self.key_to_entry.len()
    }

    fn cap(&self) -> usize {
        self.cap
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        LfuCache::remove(self, key)
    }

    fn clear(&mut self) {
        LfuCache::clear(self);
    }

    fn peek(&self, key: &K) -> Option<&V> {
        LfuCache::peek(self, key)
    }

    fn contains_key(&self, key: &K) -> bool {
        LfuCache::contains_key(self, key)
    }

    fn resize(&mut self, new_cap: usize) {
        LfuCache::resize(self, new_cap);
    }
}

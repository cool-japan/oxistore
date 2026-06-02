/// Full Adaptive Replacement Cache (ARC) implementation with TTL support.
///
/// Implements the algorithm from:
///   Megiddo, N. & Modha, D.S. (2003). ARC: A Self-Tuning, Low Overhead
///   Replacement Cache. Proceedings of FAST'03.
///
/// ## Data structures
///
/// Four `LinkedHashMap`s (all with back = MRU, front = LRU):
/// - `t1`: recently seen exactly once (recency).
/// - `t2`: seen at least twice (frequency).
/// - `b1`: ghost entries for t1 (keys only, `()` values).
/// - `b2`: ghost entries for t2 (keys only, `()` values).
///
/// `p` is the adaptive target size for `|t1|` (0..=cap).
///
/// TTL support is provided via lazy expiry: any access that encounters an
/// expired entry removes it and treats it as a miss.
use std::hash::Hash;

use hashlink::LinkedHashMap;

use crate::{Cache, CacheEntry};

/// Adaptive Replacement Cache with configurable capacity and optional per-entry TTL.
///
/// # Type parameters
///
/// - `K`: key type -- must be `Eq + Hash + Clone`.
/// - `V`: value type.
pub struct ArcCache<K, V> {
    /// Recently seen once (recency list).
    t1: LinkedHashMap<K, CacheEntry<V>, ahash::RandomState>,
    /// Seen at least twice (frequency list).
    t2: LinkedHashMap<K, CacheEntry<V>, ahash::RandomState>,
    /// Ghost keys evicted from t1.
    b1: LinkedHashMap<K, (), ahash::RandomState>,
    /// Ghost keys evicted from t2.
    b2: LinkedHashMap<K, (), ahash::RandomState>,
    /// Adaptive target for |t1| (0..=cap).
    p: usize,
    /// Total cache capacity (|t1| + |t2| <= cap).
    cap: usize,
}

impl<K, V> ArcCache<K, V>
where
    K: Eq + Hash + Clone,
{
    /// Create a new `ArcCache` with the given capacity.
    ///
    /// Capacity must be at least 1; a capacity of 0 is treated as 1 internally.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        ArcCache {
            t1: LinkedHashMap::with_hasher(ahash::RandomState::default()),
            t2: LinkedHashMap::with_hasher(ahash::RandomState::default()),
            b1: LinkedHashMap::with_hasher(ahash::RandomState::default()),
            b2: LinkedHashMap::with_hasher(ahash::RandomState::default()),
            p: 0,
            cap,
        }
    }

    /// Return the current adaptive target `p`.
    #[must_use]
    pub fn p(&self) -> usize {
        self.p
    }

    /// Total live-data entries (|t1| + |t2|).
    #[must_use]
    pub fn len(&self) -> usize {
        self.t1.len() + self.t2.len()
    }

    /// Return `true` if no live entries are cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Cache capacity.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Total directory size: |t1| + |t2| + |b1| + |b2|.
    fn directory_len(&self) -> usize {
        self.t1.len() + self.t2.len() + self.b1.len() + self.b2.len()
    }

    /// Check if an entry is expired and remove it from the given list.
    /// Returns `true` if the entry was expired and removed.
    fn remove_if_expired_t1(&mut self, key: &K) -> bool {
        let expired = self.t1.get(key).map(|e| e.is_expired()).unwrap_or(false);
        if expired {
            self.t1.remove(key);
        }
        expired
    }

    /// Check if an entry is expired and remove it from t2.
    /// Returns `true` if the entry was expired and removed.
    fn remove_if_expired_t2(&mut self, key: &K) -> bool {
        let expired = self.t2.get(key).map(|e| e.is_expired()).unwrap_or(false);
        if expired {
            self.t2.remove(key);
        }
        expired
    }

    // -------------------------------------------------------------------------
    // REPLACE sub-routine (Megiddo & Modha, FAST'03, Figure 4)
    // -------------------------------------------------------------------------
    fn replace(&mut self) {
        let t1_len = self.t1.len();
        let evict_from_t1 = t1_len >= self.p.max(1) && t1_len >= 1;

        if evict_from_t1 {
            if let Some((k, _v)) = self.t1.pop_front() {
                if self.directory_len() >= 2 * self.cap {
                    self.b1.pop_front();
                }
                self.b1.insert(k, ());
            }
        } else if !self.t2.is_empty() {
            if let Some((k, _v)) = self.t2.pop_front() {
                if self.directory_len() >= 2 * self.cap {
                    self.b2.pop_front();
                }
                self.b2.insert(k, ());
            }
        } else if !self.t1.is_empty() {
            if let Some((k, _v)) = self.t1.pop_front() {
                if self.directory_len() >= 2 * self.cap {
                    self.b1.pop_front();
                }
                self.b1.insert(k, ());
            }
        }
    }

    /// Insert an entry (with expiry) into the cache.
    fn insert_entry(&mut self, key: K, entry: CacheEntry<V>) -> Option<V> {
        // If key is already in t1 or t2, update value and promote.
        if self.t1.contains_key(&key) {
            let old = self.t1.remove(&key).expect("key confirmed in t1");
            self.t2.insert(key, entry);
            return Some(old.value);
        }
        if self.t2.contains_key(&key) {
            return self.t2.insert(key, entry).map(|e| e.value);
        }

        // Ghost hit in b1: increase p (bias toward recency), evict if needed,
        // then insert into MRU(t2).
        if self.b1.remove(&key).is_some() {
            let b1_len = self.b1.len().max(1);
            let b2_len = self.b2.len();
            let delta = (b2_len / b1_len).max(1);
            self.p = (self.p + delta).min(self.cap);
            // Ensure live count stays within cap before inserting.
            if self.t1.len() + self.t2.len() >= self.cap {
                self.replace();
            }
            self.t2.insert(key, entry);
            return None;
        }

        // Ghost hit in b2: decrease p (bias toward frequency), evict if needed,
        // then insert into MRU(t2).
        if self.b2.remove(&key).is_some() {
            let b1_len = self.b1.len();
            let b2_len = self.b2.len().max(1);
            let delta = (b1_len / b2_len).max(1);
            self.p = self.p.saturating_sub(delta);
            // Ensure live count stays within cap before inserting.
            if self.t1.len() + self.t2.len() >= self.cap {
                self.replace();
            }
            self.t2.insert(key, entry);
            return None;
        }

        // Full miss -- enforce directory / cache limits before inserting to t1.
        let live = self.t1.len() + self.t2.len();
        if live < self.cap {
            if self.directory_len() >= self.cap && self.directory_len() >= 2 * self.cap {
                if !self.b2.is_empty() {
                    self.b2.pop_front();
                } else {
                    self.b1.pop_front();
                }
            }
        } else {
            // Cache full -- need to evict.
            if self.t1.len() + self.b1.len() == self.cap {
                if !self.t1.is_empty() {
                    self.replace();
                } else {
                    self.b1.pop_front();
                }
            } else {
                if self.directory_len() >= 2 * self.cap {
                    if !self.b2.is_empty() {
                        self.b2.pop_front();
                    } else if !self.b1.is_empty() {
                        self.b1.pop_front();
                    }
                }
                self.replace();
            }
        }

        self.t1.insert(key, entry);
        None
    }

    /// Look up `key` and return a reference to its value, promoting as needed.
    ///
    /// Expired entries are lazily removed and treated as misses.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        // Case 1a: hit in t1 -- check expiry, then promote to MRU(t2).
        if self.t1.contains_key(key) {
            if self.remove_if_expired_t1(key) {
                return None;
            }
            let (k, v) = self.t1.remove_entry(key).expect("key in t1");
            self.t2.insert(k, v);
            return self.t2.get(key).map(|e| &e.value);
        }

        // Case 1b: hit in t2 -- check expiry, then move to MRU position.
        if self.t2.contains_key(key) {
            if self.remove_if_expired_t2(key) {
                return None;
            }
            if let Some(v) = self.t2.remove(key) {
                self.t2.insert(key.clone(), v);
            }
            return self.t2.get(key).map(|e| &e.value);
        }

        // Case 2: ghost hit in b1 -- adapt p upward.
        if self.b1.contains_key(key) {
            let b1_len = self.b1.len().max(1);
            let b2_len = self.b2.len();
            let delta = (b2_len / b1_len).max(1);
            self.p = (self.p + delta).min(self.cap);
            self.replace();
            return None;
        }

        // Case 3: ghost hit in b2 -- adapt p downward.
        if self.b2.contains_key(key) {
            let b1_len = self.b1.len();
            let b2_len = self.b2.len().max(1);
            let delta = (b1_len / b2_len).max(1);
            self.p = self.p.saturating_sub(delta);
            self.replace();
            return None;
        }

        // Case 4: complete miss.
        None
    }

    /// Insert `key` -> `value` into the cache (no TTL).
    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        self.insert_entry(key, CacheEntry::new(value))
    }

    /// Insert `key` -> `value` into the cache with a TTL.
    pub fn put_with_ttl(&mut self, key: K, value: V, ttl: std::time::Duration) -> Option<V> {
        self.insert_entry(key, CacheEntry::with_ttl(value, ttl))
    }

    /// Read a value without promoting it.
    ///
    /// Checks t1 and t2 but does NOT move entries or trigger ghost adaptation.
    /// Expired entries return `None` (removal happens on next mutable access).
    #[must_use]
    pub fn peek(&self, key: &K) -> Option<&V> {
        let from_t1 = self
            .t1
            .get(key)
            .and_then(|e| if e.is_expired() { None } else { Some(&e.value) });
        if from_t1.is_some() {
            return from_t1;
        }
        self.t2
            .get(key)
            .and_then(|e| if e.is_expired() { None } else { Some(&e.value) })
    }

    /// Return `true` if `key` is present in t1 or t2 and not expired (without promotion).
    #[must_use]
    pub fn contains_key(&self, key: &K) -> bool {
        let in_t1 = self.t1.get(key).map(|e| !e.is_expired()).unwrap_or(false);
        let in_t2 = self.t2.get(key).map(|e| !e.is_expired()).unwrap_or(false);
        in_t1 || in_t2
    }

    /// Remove the entry for `key` from the cache.
    ///
    /// Returns the value if it was present in t1 or t2.  Also removes any
    /// ghost entry from b1/b2.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(e) = self.t1.remove(key) {
            return Some(e.value);
        }
        if let Some(e) = self.t2.remove(key) {
            return Some(e.value);
        }
        // Also clear ghost entries to prevent stale ghost hits.
        self.b1.remove(key);
        self.b2.remove(key);
        None
    }

    /// Remove all entries from the cache (live + ghost).
    pub fn clear(&mut self) {
        self.t1.clear();
        self.t2.clear();
        self.b1.clear();
        self.b2.clear();
        self.p = 0;
    }

    /// Dynamically resize the cache capacity.
    ///
    /// If `new_cap` is smaller than the current live count, entries are
    /// evicted via the REPLACE routine until the count fits.
    pub fn resize(&mut self, new_cap: usize) {
        let new_cap = new_cap.max(1);
        self.cap = new_cap;
        // If p exceeds the new cap, clamp it.
        if self.p > self.cap {
            self.p = self.cap;
        }
        // Evict until live count fits.
        while self.len() > self.cap {
            self.replace();
        }
    }
}

impl<K: std::fmt::Debug, V> std::fmt::Debug for ArcCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArcCache")
            .field("cap", &self.cap)
            .field("p", &self.p)
            .field("t1_len", &self.t1.len())
            .field("t2_len", &self.t2.len())
            .field("b1_len", &self.b1.len())
            .field("b2_len", &self.b2.len())
            .finish_non_exhaustive()
    }
}

impl<K, V> Cache<K, V> for ArcCache<K, V>
where
    K: Eq + Hash + Clone,
{
    fn get(&mut self, key: &K) -> Option<&V> {
        ArcCache::get(self, key)
    }

    fn put(&mut self, key: K, value: V) -> Option<V> {
        ArcCache::put(self, key, value)
    }

    fn put_with_ttl(&mut self, key: K, value: V, ttl: std::time::Duration) -> Option<V> {
        ArcCache::put_with_ttl(self, key, value, ttl)
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn cap(&self) -> usize {
        self.cap
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        ArcCache::remove(self, key)
    }

    fn clear(&mut self) {
        ArcCache::clear(self);
    }

    fn peek(&self, key: &K) -> Option<&V> {
        ArcCache::peek(self, key)
    }

    fn contains_key(&self, key: &K) -> bool {
        ArcCache::contains_key(self, key)
    }

    fn resize(&mut self, new_cap: usize) {
        ArcCache::resize(self, new_cap);
    }

    fn values(&self) -> Vec<&V> {
        let t1_vals = self
            .t1
            .values()
            .filter(|e| !e.is_expired())
            .map(|e| &e.value);
        let t2_vals = self
            .t2
            .values()
            .filter(|e| !e.is_expired())
            .map(|e| &e.value);
        t1_vals.chain(t2_vals).collect()
    }
}

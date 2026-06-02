/// Pure-Rust LRU cache backed by `hashlink::LinkedHashMap`.
///
/// The map treats the *back* of the linked list as most-recently-used (MRU)
/// and the *front* as least-recently-used (LRU), following `LinkedHashMap`'s
/// ordering convention (back = most recently inserted/promoted).
///
/// On `get` the entry is moved to the back (MRU) so frequently accessed items
/// are protected from eviction.  On `put`, if the cache is already at capacity
/// the front entry (LRU) is evicted before inserting the new item.
///
/// TTL support is provided via [`LruCache::put_with_ttl`].  Expiry is checked
/// lazily on every access — no background thread is needed.
use std::hash::Hash;

use hashlink::LinkedHashMap;

use crate::{Cache, CacheEntry};

/// An LRU cache with a fixed capacity and optional per-entry TTL.
///
/// # Type parameters
///
/// - `K`: key type -- must be `Eq + Hash`.
/// - `V`: value type.
pub struct LruCache<K, V> {
    map: LinkedHashMap<K, CacheEntry<V>, ahash::RandomState>,
    cap: usize,
}

impl<K, V> LruCache<K, V>
where
    K: Eq + Hash,
{
    /// Create a new `LruCache` with the given capacity.
    ///
    /// A capacity of `0` is valid but every `put` will immediately evict the
    /// newly inserted entry on the next insertion.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        LruCache {
            map: LinkedHashMap::with_hasher(ahash::RandomState::default()),
            cap,
        }
    }

    /// Insert `key` -> `value` with an expiry time derived from the raw entry.
    ///
    /// Returns the evicted value (for new-key insertions at capacity) or `None`
    /// (for key updates — the old value is not returned, matching original behavior).
    fn insert_entry(&mut self, key: K, entry: CacheEntry<V>) -> Option<V> {
        if self.map.contains_key(&key) {
            // Key update: replace in place and promote to MRU.  No eviction.
            self.map.insert(key, entry);
            return None;
        }

        // New key: evict LRU if at capacity.
        let evicted = if self.cap > 0 && self.map.len() >= self.cap {
            self.map.pop_front().map(|(_, e)| e.value)
        } else {
            None
        };

        self.map.insert(key, entry);
        evicted
    }

    /// Return the value associated with `key`, moving it to the MRU position.
    ///
    /// If the entry has expired, it is removed and `None` is returned.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        // First check expiry without promoting.
        let expired = self.map.get(key).map(|e| e.is_expired()).unwrap_or(false);

        if expired {
            self.map.remove(key);
            return None;
        }

        // Move the entry to back (MRU) using the raw entry API.
        match self.map.raw_entry_mut().from_key(key) {
            hashlink::linked_hash_map::RawEntryMut::Occupied(mut occ) => {
                occ.to_back();
                Some(&occ.into_mut().value)
            }
            hashlink::linked_hash_map::RawEntryMut::Vacant(_) => None,
        }
    }

    /// Insert or update `key` -> `value` (no TTL), evicting the LRU entry if at capacity.
    ///
    /// - If `key` already exists: updates the value and promotes to MRU; no eviction.
    /// - If `key` is new and the cache is at capacity: evicts the LRU entry first.
    ///
    /// Returns the evicted value (not the replaced value on key updates).
    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        self.insert_entry(key, CacheEntry::new(value))
    }

    /// Insert or update `key` -> `value` with a TTL.
    pub fn put_with_ttl(&mut self, key: K, value: V, ttl: std::time::Duration) -> Option<V> {
        self.insert_entry(key, CacheEntry::with_ttl(value, ttl))
    }

    /// Return the number of entries currently in the cache (including unexpired).
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Return `true` if the cache contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Return the maximum capacity of the cache.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Return `true` if `key` is present and not expired (without promoting).
    #[must_use]
    pub fn contains(&self, key: &K) -> bool {
        match self.map.get(key) {
            Some(e) => !e.is_expired(),
            None => false,
        }
    }

    /// Read the value for `key` without promoting it to MRU.
    ///
    /// Returns `None` if the key is not present or has expired.
    /// Note: this method takes `&self`, so it cannot remove the expired entry;
    /// the removal happens lazily on the next mutable access.
    #[must_use]
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.map
            .get(key)
            .and_then(|e| if e.is_expired() { None } else { Some(&e.value) })
    }

    /// Remove the entry for `key`, returning its value if present and not expired.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.map.remove(key).map(|e| e.value)
    }

    /// Remove all entries from the cache.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Dynamically resize the cache capacity.
    ///
    /// If `new_cap` is smaller than the current length, LRU entries are
    /// evicted until the length fits.
    pub fn resize(&mut self, new_cap: usize) {
        self.cap = new_cap;
        while self.cap > 0 && self.map.len() > self.cap {
            self.map.pop_front();
        }
    }

    /// Return an iterator over `(&K, &V)` pairs in LRU-to-MRU order
    /// (front to back, oldest to newest).
    ///
    /// Expired entries are included in the iterator; callers should check
    /// expiry if needed.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.map.iter().map(|(k, e)| (k, &e.value))
    }

    /// Return an entry handle for `key`.
    ///
    /// - [`Entry::Occupied`] if the key is present (and not expired).
    /// - [`Entry::Vacant`] otherwise.
    ///
    /// The entry API mirrors `HashMap::entry`, enabling efficient
    /// insert-or-modify patterns without double hash lookups in most cases.
    pub fn entry(&mut self, key: K) -> Entry<'_, K, V>
    where
        K: Clone,
    {
        if self.contains(&key) {
            Entry::Occupied(OccupiedEntry { cache: self, key })
        } else {
            Entry::Vacant(VacantEntry { cache: self, key })
        }
    }
}

impl<K: std::fmt::Debug, V> std::fmt::Debug for LruCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LruCache")
            .field("capacity", &self.cap)
            .field("length", &self.map.len())
            .finish_non_exhaustive()
    }
}

impl<K: Eq + Hash, V> From<Vec<(K, V)>> for LruCache<K, V> {
    /// Build an `LruCache` from a vector of `(key, value)` pairs.
    ///
    /// The cache capacity is set to `pairs.len().max(1)` so that all pairs
    /// fit without eviction.
    fn from(pairs: Vec<(K, V)>) -> Self {
        let cap = pairs.len().max(1);
        let mut cache = LruCache::new(cap);
        for (k, v) in pairs {
            cache.put(k, v);
        }
        cache
    }
}

// ---------------------------------------------------------------------------
// Entry API
// ---------------------------------------------------------------------------

/// A view into a single entry in an [`LruCache`], either occupied or vacant.
///
/// Created by [`LruCache::entry`].
pub enum Entry<'a, K, V> {
    /// The cache has an entry for the key.
    Occupied(OccupiedEntry<'a, K, V>),
    /// The cache does not have an entry for the key.
    Vacant(VacantEntry<'a, K, V>),
}

/// A handle to an occupied entry in an [`LruCache`].
pub struct OccupiedEntry<'a, K, V> {
    cache: &'a mut LruCache<K, V>,
    key: K,
}

/// A handle to a vacant entry in an [`LruCache`].
pub struct VacantEntry<'a, K, V> {
    cache: &'a mut LruCache<K, V>,
    key: K,
}

impl<'a, K: Eq + Hash + Clone, V> OccupiedEntry<'a, K, V> {
    /// Return a reference to the entry's key.
    pub fn key(&self) -> &K {
        &self.key
    }

    /// Return a reference to the entry's value without promoting to MRU.
    ///
    /// # Panics
    ///
    /// Panics if the entry is no longer present (which cannot happen under
    /// normal use, since the entry was checked before constructing this handle).
    pub fn get(&self) -> &V {
        self.cache.peek(&self.key).expect("occupied entry exists")
    }

    /// Remove the entry and return its value.
    ///
    /// # Panics
    ///
    /// Panics if the entry is no longer present (which cannot happen under
    /// normal use).
    pub fn remove(self) -> V {
        self.cache.remove(&self.key).expect("occupied entry exists")
    }
}

impl<'a, K: Eq + Hash + Clone, V> VacantEntry<'a, K, V> {
    /// Return a reference to the key that would be inserted.
    pub fn key(&self) -> &K {
        &self.key
    }

    /// Insert `value` under this key and return a reference to the stored value.
    pub fn insert(self, value: V) -> &'a V {
        let VacantEntry { cache, key } = self;
        cache.put(key.clone(), value);
        cache.peek(&key).expect("key was just inserted")
    }
}

impl<K, V> Cache<K, V> for LruCache<K, V>
where
    K: Eq + Hash,
{
    fn get(&mut self, key: &K) -> Option<&V> {
        LruCache::get(self, key)
    }

    fn put(&mut self, key: K, value: V) -> Option<V> {
        LruCache::put(self, key, value)
    }

    fn put_with_ttl(&mut self, key: K, value: V, ttl: std::time::Duration) -> Option<V> {
        LruCache::put_with_ttl(self, key, value, ttl)
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn cap(&self) -> usize {
        self.cap
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        LruCache::remove(self, key)
    }

    fn clear(&mut self) {
        LruCache::clear(self);
    }

    fn peek(&self, key: &K) -> Option<&V> {
        LruCache::peek(self, key)
    }

    fn contains_key(&self, key: &K) -> bool {
        LruCache::contains(self, key)
    }

    fn resize(&mut self, new_cap: usize) {
        LruCache::resize(self, new_cap);
    }

    fn values(&self) -> Vec<&V> {
        self.map
            .values()
            .filter(|e| !e.is_expired())
            .map(|e| &e.value)
            .collect()
    }
}

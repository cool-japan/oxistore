//! Write-through and write-back cache adapters.
//!
//! Both adapters combine a [`Cache<Vec<u8>, Vec<u8>>`] with an
//! [`oxistore_core::KvStore`] to provide transparent persistence.
//!
//! ## Write-Through
//!
//! [`WriteThroughCache`] ensures every `put` is immediately written to the
//! backing store.  Cache misses on `get` are populated from the store.
//!
//! ## Write-Back
//!
//! [`WriteBackCache`] writes to the cache immediately but defers flushing to
//! the store until an explicit [`WriteBackCache::flush`] call.  If the inner
//! cache evicts a dirty entry, the entry is flushed to the store synchronously
//! to avoid silent data loss.

use std::collections::HashSet;

use oxistore_core::{KvStore, StoreError};

use crate::Cache;

// ---------------------------------------------------------------------------
// WriteThroughCache
// ---------------------------------------------------------------------------

/// A cache adapter that propagates writes to a backing [`KvStore`] immediately.
///
/// - `put(k, v)` → writes to both the cache **and** the store.
/// - `get(k)` → returns from cache if present; on miss, fetches from the store,
///   populates the cache, and returns the value.
/// - `remove(k)` → removes from both cache and store.
///
/// # Type parameters
///
/// - `S`: a [`KvStore`] implementor.
/// - `C`: a `Cache<Vec<u8>, Vec<u8>>` implementor.
pub struct WriteThroughCache<S, C> {
    store: S,
    cache: C,
}

impl<S, C> WriteThroughCache<S, C>
where
    S: KvStore,
    C: Cache<Vec<u8>, Vec<u8>>,
{
    /// Create a new write-through cache adapter.
    pub fn new(store: S, cache: C) -> Self {
        WriteThroughCache { store, cache }
    }

    /// Borrow the inner store.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Borrow the inner cache.
    pub fn cache(&self) -> &C {
        &self.cache
    }

    /// Look up `key`: cache first, then store on miss.
    ///
    /// On a store hit the value is populated back into the cache.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        // Cache hit (returns a reference — clone to avoid borrow issues).
        if let Some(v) = self.cache.get(&key.to_vec()) {
            return Ok(Some(v.clone()));
        }
        // Cache miss → try store.
        match self.store.get(key)? {
            Some(v) => {
                // Populate cache.
                self.cache.put(key.to_vec(), v.clone());
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    /// Insert `key` → `value` into both cache and store.
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), StoreError> {
        self.store.put(&key, &value)?;
        self.cache.put(key, value);
        Ok(())
    }

    /// Remove `key` from both cache and store.
    pub fn remove(&mut self, key: &[u8]) -> Result<(), StoreError> {
        self.cache.remove(&key.to_vec());
        self.store.delete(key)?;
        Ok(())
    }

    /// Return the number of live cache entries.
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }
}

// ---------------------------------------------------------------------------
// WriteBackCache
// ---------------------------------------------------------------------------

/// A cache adapter that defers writes to the backing [`KvStore`].
///
/// Writes are buffered in the cache and marked as dirty.  The store is not
/// updated until:
///
/// 1. [`WriteBackCache::flush`] is called explicitly, or
/// 2. The inner cache evicts a dirty entry (to avoid silent data loss).
///
/// # Eviction of dirty entries
///
/// The `Cache` trait has no eviction callback.  This adapter uses a
/// cooperative model: before calling `put` on the inner cache, it checks
/// whether the cache is full and peeks at the current LRU entry.  If the
/// entry to be evicted is dirty it is flushed first.  This is a best-effort
/// approach — the adapter introspects the cache state before insertion; it
/// does **not** hook into the cache's internal eviction path.
///
/// # Type parameters
///
/// - `S`: a [`KvStore`] implementor.
/// - `C`: a `Cache<Vec<u8>, Vec<u8>>` implementor.
pub struct WriteBackCache<S, C> {
    store: S,
    cache: C,
    dirty: HashSet<Vec<u8>>,
}

impl<S, C> WriteBackCache<S, C>
where
    S: KvStore,
    C: Cache<Vec<u8>, Vec<u8>>,
{
    /// Create a new write-back cache adapter with an empty dirty set.
    pub fn new(store: S, cache: C) -> Self {
        WriteBackCache {
            store,
            cache,
            dirty: HashSet::new(),
        }
    }

    /// Borrow the inner store.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Borrow the inner cache.
    pub fn cache(&self) -> &C {
        &self.cache
    }

    /// Return the number of keys that have unflushed writes.
    pub fn dirty_count(&self) -> usize {
        self.dirty.len()
    }

    /// Look up `key`: cache first, then store on miss.
    ///
    /// Store hits are **not** inserted back into the cache (read-around policy)
    /// to avoid polluting the cache with cold data that has already been
    /// committed.  Callers that want read-population can call `put` after `get`.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        if let Some(v) = self.cache.get(&key.to_vec()) {
            return Ok(Some(v.clone()));
        }
        // Cache miss → fetch from store (read-around, do not populate cache).
        self.store.get(key)
    }

    /// Insert `key` → `value` into the cache and mark dirty.
    ///
    /// The store is **not** updated immediately.
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), StoreError> {
        // Pre-eviction check: if the cache is at capacity and the to-be-evicted
        // entry is dirty, flush it before it disappears.
        self.flush_if_eviction_imminent(&key)?;

        self.dirty.insert(key.clone());
        self.cache.put(key, value);
        Ok(())
    }

    /// Remove `key` from the cache and dirty set, and delete from the store.
    pub fn remove(&mut self, key: &[u8]) -> Result<(), StoreError> {
        let key_vec = key.to_vec();
        self.cache.remove(&key_vec);
        self.dirty.remove(&key_vec);
        self.store.delete(key)?;
        Ok(())
    }

    /// Flush all dirty keys to the store and clear the dirty set.
    ///
    /// For each dirty key the value is read from the cache.  If the key is no
    /// longer in the cache (it was evicted and presumably already flushed via
    /// the pre-eviction hook), it is skipped.
    pub fn flush(&mut self) -> Result<(), StoreError> {
        let dirty_keys: Vec<Vec<u8>> = self.dirty.iter().cloned().collect();
        for key in dirty_keys {
            if let Some(val) = self.cache.peek(&key) {
                self.store.put(&key, val)?;
            }
            // If peek returns None the key was evicted; it was flushed at eviction time.
        }
        self.dirty.clear();
        Ok(())
    }

    /// Check whether inserting a new key will trigger eviction of a dirty entry.
    ///
    /// This is called *before* every `put`.  If the cache is full and the front
    /// (LRU) entry is dirty, it is flushed to the store now so that the value
    /// is not lost when the inner cache evicts it.
    fn flush_if_eviction_imminent(&mut self, incoming_key: &[u8]) -> Result<(), StoreError> {
        // Only relevant if the cache is at or beyond capacity and the new key
        // is not already present (updates don't cause eviction).
        if self.cache.contains_key(&incoming_key.to_vec()) {
            return Ok(());
        }
        if self.cache.len() < self.cache.cap() {
            return Ok(());
        }
        // The cache is full and the new key is novel — eviction will happen.
        // Find the LRU candidate via peek on the order-tracking structure.
        // Because we don't have direct access to the cache's internal order,
        // we iterate the dirty set and check each dirty key: any dirty key
        // currently at the LRU position might be evicted.  The simplest safe
        // approach: flush all dirty entries now (conservative but correct).
        // This avoids requiring the inner Cache trait to expose its eviction order.
        let dirty_keys: Vec<Vec<u8>> = self.dirty.iter().cloned().collect();
        for key in dirty_keys {
            if let Some(val) = self.cache.peek(&key) {
                self.store.put(&key, val)?;
            }
        }
        // We do NOT clear dirty here because the actual eviction hasn't happened yet;
        // after flush the entries are still in the cache.  They'll be cleared on
        // the next explicit flush() call.  This is safe because store.put is idempotent.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CacheableKvStore
// ---------------------------------------------------------------------------

/// A read-through cache adapter that wraps a [`KvStore`] with a [`Cache`].
///
/// - `get(k)` — checks the cache first; on a miss, fetches from the store,
///   populates the cache, and returns the value.  The mutex lock is **not**
///   held while calling the underlying store, so concurrent readers are never
///   blocked by store I/O.
/// - `put(k, v)` — writes to the store immediately and then invalidates the
///   cached entry to prevent stale reads.
/// - `delete(k)` — deletes from the store and invalidates the cached entry.
/// - All other [`KvStore`] methods (transactions, snapshots, iteration, …)
///   delegate directly to the inner store without cache involvement.
///
/// # Type parameters
///
/// - `S`: a [`KvStore`] implementor.
/// - `C`: a `Cache<Vec<u8>, Vec<u8>>` implementor that is also `Send`.
pub struct CacheableKvStore<S, C> {
    store: S,
    cache: std::sync::Mutex<C>,
}

impl<S, C> CacheableKvStore<S, C> {
    /// Create a new `CacheableKvStore` wrapping `store` and `cache`.
    pub fn new(store: S, cache: C) -> Self {
        CacheableKvStore {
            store,
            cache: std::sync::Mutex::new(cache),
        }
    }
}

impl<S, C> KvStore for CacheableKvStore<S, C>
where
    S: KvStore,
    C: Cache<Vec<u8>, Vec<u8>> + Send,
{
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        // CRITICAL: lock → check cache → if hit return clone → drop lock.
        // Do NOT hold the lock while calling store.get.
        let cached = {
            let mut guard = self
                .cache
                .lock()
                .map_err(|e| StoreError::Other(format!("cache lock poisoned: {e}")))?;
            guard.get(&key.to_vec()).cloned()
        };
        if let Some(v) = cached {
            return Ok(Some(v));
        }
        // Cache miss — fetch from store without holding the lock.
        let from_store = self.store.get(key)?;
        if let Some(ref v) = from_store {
            // Re-acquire lock to insert the fetched value.
            let mut guard = self
                .cache
                .lock()
                .map_err(|e| StoreError::Other(format!("cache lock poisoned: {e}")))?;
            guard.put(key.to_vec(), v.clone());
        }
        Ok(from_store)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        // Write to store first (durability), then invalidate cache entry.
        self.store.put(key, value)?;
        let mut guard = self
            .cache
            .lock()
            .map_err(|e| StoreError::Other(format!("cache lock poisoned: {e}")))?;
        guard.remove(&key.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.store.delete(key)?;
        let mut guard = self
            .cache
            .lock()
            .map_err(|e| StoreError::Other(format!("cache lock poisoned: {e}")))?;
        guard.remove(&key.to_vec());
        Ok(())
    }

    fn range<'a>(
        &'a self,
        lo: &[u8],
        hi: &[u8],
    ) -> Result<oxistore_core::RangeIter<'a>, StoreError> {
        self.store.range(lo, hi)
    }

    fn iter<'a>(&'a self) -> Result<oxistore_core::RangeIter<'a>, StoreError> {
        self.store.iter()
    }

    fn transaction(&self) -> Result<Box<dyn oxistore_core::KvTxn + '_>, StoreError> {
        self.store.transaction()
    }

    fn snapshot(&self) -> Result<Box<dyn oxistore_core::KvSnapshot + '_>, StoreError> {
        self.store.snapshot()
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.store.flush()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LruCache;
    use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // Minimal in-memory KvStore for tests.
    #[derive(Default, Debug)]
    struct MemStore(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

    impl KvStore for MemStore {
        fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(self.0.lock().expect("lock").get(key).cloned())
        }

        fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
            self.0
                .lock()
                .expect("lock")
                .insert(key.to_vec(), value.to_vec());
            Ok(())
        }

        fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
            self.0.lock().expect("lock").remove(key);
            Ok(())
        }

        fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
            let guard = self.0.lock().expect("lock");
            let lo = lo.to_vec();
            let hi = hi.to_vec();
            let pairs: Vec<_> = guard
                .iter()
                .filter(|(k, _)| **k >= lo && **k < hi)
                .map(|(k, v)| Ok((k.clone(), v.clone())))
                .collect();
            drop(guard);
            Ok(Box::new(pairs.into_iter()))
        }

        fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
            Err(StoreError::Other("MemStore: no txn".to_string()))
        }

        fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
            Err(StoreError::Other("MemStore: no snapshot".to_string()))
        }

        fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
            let guard = self.0.lock().expect("lock");
            let mut pairs: Vec<(Vec<u8>, Vec<u8>)> =
                guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            drop(guard);
            pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
            Ok(Box::new(pairs.into_iter().map(Ok)))
        }

        fn flush(&self) -> Result<(), StoreError> {
            Ok(())
        }
    }

    // ── WriteThroughCache tests ───────────────────────────────────────────────

    #[test]
    fn write_through_put_flushes_to_store() {
        let store = MemStore::default();
        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wt = WriteThroughCache::new(store, cache);

        wt.put(b"key".to_vec(), b"value".to_vec())
            .expect("put failed");

        // Verify the store was written immediately.
        let from_store = wt.store().get(b"key").expect("get failed");
        assert_eq!(from_store, Some(b"value".to_vec()));
    }

    #[test]
    fn write_through_get_hits_cache() {
        let store = MemStore::default();
        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wt = WriteThroughCache::new(store, cache);

        wt.put(b"k".to_vec(), b"v".to_vec()).expect("put");
        let v = wt.get(b"k").expect("get");
        assert_eq!(v, Some(b"v".to_vec()));
    }

    #[test]
    fn write_through_get_miss_populates_from_store() {
        let store = MemStore::default();
        // Pre-populate store directly.
        store.put(b"existing", b"from_store").expect("store put");

        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wt = WriteThroughCache::new(store, cache);

        // Cache is empty; should fetch from store and return value.
        let v = wt.get(b"existing").expect("get");
        assert_eq!(v, Some(b"from_store".to_vec()));

        // Now the cache should be populated — second get should hit the cache.
        assert_eq!(wt.cache_len(), 1);
    }

    #[test]
    fn write_through_remove_clears_store() {
        let store = MemStore::default();
        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wt = WriteThroughCache::new(store, cache);

        wt.put(b"rm_key".to_vec(), b"rm_val".to_vec()).expect("put");
        wt.remove(b"rm_key").expect("remove");

        let from_store = wt.store().get(b"rm_key").expect("store get");
        assert!(from_store.is_none());
    }

    #[test]
    fn write_through_get_miss_absent_in_store() {
        let store = MemStore::default();
        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wt = WriteThroughCache::new(store, cache);

        let v = wt.get(b"no_such_key").expect("get");
        assert!(v.is_none());
    }

    // ── WriteBackCache tests ──────────────────────────────────────────────────

    #[test]
    fn write_back_put_deferred() {
        let store = MemStore::default();
        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wb = WriteBackCache::new(store, cache);

        wb.put(b"lazy".to_vec(), b"write".to_vec()).expect("put");

        // Store should NOT have the value yet.
        let from_store = wb.store().get(b"lazy").expect("store get");
        assert!(from_store.is_none());
        assert_eq!(wb.dirty_count(), 1);
    }

    #[test]
    fn write_back_flush_persists() {
        let store = MemStore::default();
        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wb = WriteBackCache::new(store, cache);

        wb.put(b"a".to_vec(), b"1".to_vec()).expect("put");
        wb.put(b"b".to_vec(), b"2".to_vec()).expect("put");

        wb.flush().expect("flush");

        assert_eq!(wb.dirty_count(), 0);
        assert_eq!(
            wb.store().get(b"a").expect("store get a"),
            Some(b"1".to_vec())
        );
        assert_eq!(
            wb.store().get(b"b").expect("store get b"),
            Some(b"2".to_vec())
        );
    }

    #[test]
    fn write_back_get_hits_cache() {
        let store = MemStore::default();
        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wb = WriteBackCache::new(store, cache);

        wb.put(b"key".to_vec(), b"val".to_vec()).expect("put");
        let v = wb.get(b"key").expect("get");
        assert_eq!(v, Some(b"val".to_vec()));
    }

    #[test]
    fn write_back_get_misses_to_store() {
        let store = MemStore::default();
        store.put(b"persistent", b"data").expect("store put");

        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wb = WriteBackCache::new(store, cache);

        let v = wb.get(b"persistent").expect("get");
        assert_eq!(v, Some(b"data".to_vec()));
    }

    #[test]
    fn write_back_remove_deletes_from_store() {
        let store = MemStore::default();
        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
        let mut wb = WriteBackCache::new(store, cache);

        wb.put(b"del".to_vec(), b"gone".to_vec()).expect("put");
        wb.flush().expect("flush");
        wb.remove(b"del").expect("remove");

        assert!(wb.store().get(b"del").expect("store get").is_none());
        assert_eq!(wb.dirty_count(), 0);
    }

    #[test]
    fn write_back_dirty_eviction_flushes() {
        // Cache capacity = 2; insert 3 keys → eviction of 1st dirty key.
        let store = MemStore::default();
        let cache = LruCache::<Vec<u8>, Vec<u8>>::new(2);
        let mut wb = WriteBackCache::new(store, cache);

        wb.put(b"first".to_vec(), b"v1".to_vec()).expect("put 1");
        wb.put(b"second".to_vec(), b"v2".to_vec()).expect("put 2");
        // Inserting "third" should trigger eviction of "first" (LRU).
        // Our pre-eviction hook flushes all dirty keys conservatively.
        wb.put(b"third".to_vec(), b"v3".to_vec()).expect("put 3");

        // After explicit flush everything should be in the store.
        wb.flush().expect("flush");

        // At minimum "second" and "third" are in the store.
        let v2 = wb.store().get(b"second").expect("store get second");
        let v3 = wb.store().get(b"third").expect("store get third");
        assert_eq!(v2, Some(b"v2".to_vec()));
        assert_eq!(v3, Some(b"v3".to_vec()));
    }
}

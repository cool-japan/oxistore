use oxistore_cache::{ArcCache, Cache, CacheableKvStore, LruCache, SyncCache};
use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
use std::collections::HashMap;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Minimal in-memory KvStore for integration tests
// ---------------------------------------------------------------------------

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

// =================== LRU New Features ===================

#[test]
fn lru_remove() {
    let mut cache = LruCache::new(3);
    cache.put(1, "a");
    cache.put(2, "b");
    assert_eq!(cache.remove(&1), Some("a"));
    assert_eq!(cache.len(), 1);
    assert!(cache.get(&1).is_none());
}

#[test]
fn lru_clear() {
    let mut cache = LruCache::new(3);
    cache.put(1, "a");
    cache.put(2, "b");
    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[test]
fn lru_peek_no_promotion() {
    let mut cache = LruCache::new(2);
    cache.put(1, "a");
    cache.put(2, "b");
    // Peek at 1 -- should NOT promote it.
    assert_eq!(cache.peek(&1), Some(&"a"));
    // Insert 3 -- should evict 1 (still LRU) since peek didn't promote.
    cache.put(3, "c");
    assert!(cache.get(&1).is_none());
    assert_eq!(cache.get(&2), Some(&"b"));
}

#[test]
fn lru_contains_key() {
    let mut cache: LruCache<u32, &str> = LruCache::new(3);
    cache.put(1, "a");
    assert!(Cache::contains_key(&cache, &1));
    assert!(!Cache::contains_key(&cache, &2));
}

#[test]
fn lru_resize_down() {
    let mut cache = LruCache::new(5);
    for i in 0..5 {
        cache.put(i, i * 10);
    }
    assert_eq!(cache.len(), 5);
    Cache::resize(&mut cache, 2);
    assert_eq!(cache.cap(), 2);
    assert_eq!(cache.len(), 2);
    // The 3 LRU entries (0, 1, 2) should be evicted; 3 and 4 remain.
    assert!(cache.get(&3).is_some());
    assert!(cache.get(&4).is_some());
}

#[test]
fn lru_resize_up() {
    let mut cache = LruCache::new(2);
    cache.put(1, "a");
    cache.put(2, "b");
    Cache::resize(&mut cache, 5);
    assert_eq!(cache.cap(), 5);
    // No evictions happened.
    assert_eq!(cache.len(), 2);
}

#[test]
fn lru_iter() {
    let mut cache = LruCache::new(3);
    cache.put(3, "c");
    cache.put(1, "a");
    cache.put(2, "b");
    let items: Vec<_> = cache.iter().collect();
    // LRU to MRU order: 3, 1, 2
    assert_eq!(items.len(), 3);
    assert_eq!(*items[0].0, 3);
    assert_eq!(*items[1].0, 1);
    assert_eq!(*items[2].0, 2);
}

// =================== ARC New Features ===================

#[test]
fn arc_remove() {
    let mut cache = ArcCache::new(3);
    cache.put(1, "a");
    cache.put(2, "b");
    assert_eq!(cache.remove(&1), Some("a"));
    assert_eq!(cache.len(), 1);
    assert!(cache.get(&1).is_none());
}

#[test]
fn arc_clear() {
    let mut cache = ArcCache::new(3);
    cache.put(1, "a");
    cache.put(2, "b");
    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
    assert_eq!(cache.p(), 0);
}

#[test]
fn arc_peek_no_promotion() {
    let mut cache = ArcCache::new(2);
    cache.put(1, "a");
    cache.put(2, "b");
    // Peek should not promote.
    assert_eq!(cache.peek(&1), Some(&"a"));
    // verify 1 is still in t1 (hasn't been moved to t2).
    assert!(cache.contains_key(&1));
}

#[test]
fn arc_contains_key() {
    let mut cache: ArcCache<u32, &str> = ArcCache::new(3);
    cache.put(1, "a");
    assert!(Cache::contains_key(&cache, &1));
    assert!(!Cache::contains_key(&cache, &2));
}

#[test]
fn arc_resize_down() {
    let mut cache = ArcCache::new(5);
    for i in 0..5 {
        cache.put(i, i * 10);
    }
    assert_eq!(cache.len(), 5);
    Cache::resize(&mut cache, 2);
    assert_eq!(cache.cap(), 2);
    assert!(cache.len() <= 2);
}

#[test]
fn arc_resize_up() {
    let mut cache = ArcCache::new(2);
    cache.put(1, "a");
    cache.put(2, "b");
    Cache::resize(&mut cache, 5);
    assert_eq!(cache.cap(), 5);
    assert_eq!(cache.len(), 2);
}

#[test]
fn arc_remove_also_clears_ghosts() {
    let mut cache = ArcCache::new(2);
    cache.put(1, "a");
    cache.put(2, "b");
    // Evict 1 to become a ghost.
    cache.put(3, "c");
    // Now explicitly remove 1 (which is a ghost).
    assert_eq!(cache.remove(&1), None);
    // 1 should not cause a ghost hit anymore.
    // Accessing 1 should be a full miss now.
    assert!(cache.get(&1).is_none());
}

// =================== CacheableKvStore Tests ===================

/// Test that a value put via the underlying store can be retrieved through
/// CacheableKvStore and that the second read is served from the cache.
#[test]
fn cacheable_kv_store_cache_hit() {
    let store = MemStore::default();
    // Pre-populate the backing store directly.
    store.put(b"key1", b"value1").expect("store put");

    let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
    let cacheable = CacheableKvStore::new(store, cache);

    // First get — should be a cache miss, fetched from store and inserted into cache.
    let v1 = cacheable.get(b"key1").expect("first get");
    assert_eq!(v1, Some(b"value1".to_vec()));

    // Second get — should hit the cache (same result).
    let v2 = cacheable.get(b"key1").expect("second get");
    assert_eq!(v2, Some(b"value1".to_vec()));
}

/// Test that a put via CacheableKvStore invalidates any cached entry so that
/// a subsequent get returns the updated value, not the stale cached one.
#[test]
fn cacheable_kv_store_invalidation() {
    let store = MemStore::default();

    let cache = LruCache::<Vec<u8>, Vec<u8>>::new(8);
    let cacheable = CacheableKvStore::new(store, cache);

    // Put initial value — populates both store and (after first get) cache.
    cacheable.put(b"key2", b"original").expect("put original");

    // First get — cache miss (put only stores to store + invalidates), fetches
    // from store and populates cache.
    let v1 = cacheable.get(b"key2").expect("first get");
    assert_eq!(v1, Some(b"original".to_vec()));

    // Overwrite the key — this should write to store and invalidate the cache.
    cacheable.put(b"key2", b"updated").expect("put updated");

    // Next get should return the new value, not the stale cached "original".
    let v2 = cacheable.get(b"key2").expect("get after update");
    assert_eq!(v2, Some(b"updated".to_vec()));
}

// =================== Slice-2 Edge Case Tests ===================

#[test]
fn lru_capacity_one() {
    let mut cache = LruCache::<i32, i32>::new(1);
    cache.put(1, 10);
    cache.put(2, 20); // evicts 1
    assert_eq!(cache.get(&1), None);
    assert_eq!(cache.get(&2), Some(&20));
    assert_eq!(cache.len(), 1);
}

#[test]
fn lru_duplicate_puts() {
    let mut cache = LruCache::<&str, i32>::new(5);
    cache.put("key", 1);
    cache.put("key", 2);
    cache.put("key", 3);
    assert_eq!(cache.get(&"key"), Some(&3));
    assert_eq!(cache.len(), 1);
}

#[test]
fn lru_get_on_empty() {
    let mut cache = LruCache::<i32, i32>::new(5);
    assert_eq!(cache.get(&42), None);
    assert!(cache.is_empty());
}

#[test]
fn lru_from_vec() {
    let pairs = vec![(1, 10), (2, 20), (3, 30)];
    let mut cache = LruCache::from(pairs);
    assert_eq!(cache.get(&1), Some(&10));
    assert_eq!(cache.get(&2), Some(&20));
    assert_eq!(cache.get(&3), Some(&30));
}

#[test]
fn lru_get_or_insert_populates() {
    let mut cache = LruCache::<i32, i32>::new(5);
    let v = Cache::get_or_insert(&mut cache, 1, || 42);
    assert_eq!(*v, 42);
    assert_eq!(cache.get(&1), Some(&42));
}

#[test]
fn lru_get_or_insert_does_not_overwrite() {
    let mut cache = LruCache::<i32, i32>::new(5);
    cache.put(1, 100);
    let v = Cache::get_or_insert(&mut cache, 1, || panic!("should not be called"));
    assert_eq!(*v, 100);
}

#[test]
fn lru_warm() {
    let mut cache = LruCache::<i32, i32>::new(10);
    Cache::warm(&mut cache, [(1, 10), (2, 20), (3, 30)]);
    assert_eq!(cache.len(), 3);
    assert_eq!(cache.get(&2), Some(&20));
}

#[test]
fn lru_values() {
    let mut cache = LruCache::<i32, i32>::new(5);
    cache.put(1, 10);
    cache.put(2, 20);
    let mut vals: Vec<i32> = Cache::values(&cache).iter().map(|&&v| v).collect();
    vals.sort();
    assert_eq!(vals, vec![10, 20]);
}

#[test]
fn lru_entry_vacant_and_occupied() {
    let mut cache = LruCache::<i32, i32>::new(5);
    // Vacant entry inserts.
    match cache.entry(1) {
        oxistore_cache::lru::Entry::Vacant(v) => {
            v.insert(42);
        }
        _ => panic!("expected vacant"),
    }
    assert_eq!(cache.get(&1), Some(&42));
    // Occupied entry gets value.
    match cache.entry(1) {
        oxistore_cache::lru::Entry::Occupied(o) => assert_eq!(*o.get(), 42),
        _ => panic!("expected occupied"),
    }
}

#[test]
fn sync_cache_concurrent() {
    use std::sync::Arc;
    use std::thread;
    let cache = Arc::new(SyncCache::new(LruCache::<i32, i32>::new(100)));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let c = Arc::clone(&cache);
            thread::spawn(move || {
                for j in 0..25 {
                    c.put(i * 25 + j, i * 25 + j);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread panicked");
    }
    assert_eq!(cache.len(), 100);
}

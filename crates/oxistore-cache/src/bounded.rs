//! Bounded-memory cache wrapper.
//!
//! [`BoundedCache`] wraps any `Cache<Vec<u8>, Vec<u8>>` and enforces a hard
//! byte-budget cap.  When a new entry would push `current_bytes` over `max_bytes`,
//! entries are evicted in insertion order (oldest first) until the budget allows
//! the new entry to be inserted.

use std::collections::VecDeque;

use crate::Cache;

/// A cache wrapper that caps total memory consumption by byte count.
///
/// Memory is tracked as `key.len() + value.len()` for every live entry.
/// When inserting a new entry would exceed `max_bytes`, the oldest entries
/// (tracked in insertion order) are evicted until there is enough budget.
///
/// # Type parameters
///
/// - `C`: an inner `Cache<Vec<u8>, Vec<u8>>` implementation.
pub struct BoundedCache<C> {
    inner: C,
    max_bytes: usize,
    current_bytes: usize,
    /// Insertion-order tracking for eviction (front = oldest).
    order: VecDeque<Vec<u8>>,
}

impl<C> BoundedCache<C>
where
    C: Cache<Vec<u8>, Vec<u8>>,
{
    /// Create a new `BoundedCache` wrapping `inner` with a hard byte budget.
    ///
    /// `max_bytes` is the maximum combined byte size of all keys and values.
    pub fn new(inner: C, max_bytes: usize) -> Self {
        BoundedCache {
            inner,
            max_bytes,
            current_bytes: 0,
            order: VecDeque::new(),
        }
    }

    /// Return the current byte usage.
    #[must_use]
    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    /// Return the maximum byte budget.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Evict entries in insertion order until there is at least `needed` bytes of
    /// headroom, or until no more entries remain.
    fn evict_until_fits(&mut self, needed: usize) {
        while self.current_bytes + needed > self.max_bytes {
            let oldest = match self.order.pop_front() {
                Some(k) => k,
                None => break,
            };
            if let Some(old_val) = self.inner.remove(&oldest) {
                let freed = oldest.len() + old_val.len();
                self.current_bytes = self.current_bytes.saturating_sub(freed);
            }
        }
    }
}

impl<C> Cache<Vec<u8>, Vec<u8>> for BoundedCache<C>
where
    C: Cache<Vec<u8>, Vec<u8>>,
{
    fn get(&mut self, key: &Vec<u8>) -> Option<&Vec<u8>> {
        self.inner.get(key)
    }

    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Option<Vec<u8>> {
        let entry_size = key.len() + value.len();

        // If inserting would exceed budget, evict oldest entries first.
        self.evict_until_fits(entry_size);

        // If the key already exists, subtract its old byte cost before re-inserting.
        if let Some(existing) = self.inner.peek(&key) {
            let old_size = key.len() + existing.len();
            self.current_bytes = self.current_bytes.saturating_sub(old_size);
            // Remove from order tracking — we'll re-add at the back.
            self.order.retain(|k| k != &key);
        }

        self.current_bytes += entry_size;
        self.order.push_back(key.clone());
        self.inner.put(key, value)
    }

    fn put_with_ttl(
        &mut self,
        key: Vec<u8>,
        value: Vec<u8>,
        ttl: std::time::Duration,
    ) -> Option<Vec<u8>> {
        let entry_size = key.len() + value.len();
        self.evict_until_fits(entry_size);

        if let Some(existing) = self.inner.peek(&key) {
            let old_size = key.len() + existing.len();
            self.current_bytes = self.current_bytes.saturating_sub(old_size);
            self.order.retain(|k| k != &key);
        }

        self.current_bytes += entry_size;
        self.order.push_back(key.clone());
        self.inner.put_with_ttl(key, value, ttl)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn cap(&self) -> usize {
        self.inner.cap()
    }

    fn remove(&mut self, key: &Vec<u8>) -> Option<Vec<u8>> {
        if let Some(val) = self.inner.remove(key) {
            let freed = key.len() + val.len();
            self.current_bytes = self.current_bytes.saturating_sub(freed);
            self.order.retain(|k| k != key);
            Some(val)
        } else {
            None
        }
    }

    fn clear(&mut self) {
        self.inner.clear();
        self.current_bytes = 0;
        self.order.clear();
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

    fn make_bounded(max_bytes: usize, cap: usize) -> BoundedCache<LruCache<Vec<u8>, Vec<u8>>> {
        BoundedCache::new(LruCache::new(cap), max_bytes)
    }

    #[test]
    fn bounded_under_budget() {
        let mut cache = make_bounded(100, 10);
        cache.put(b"key1".to_vec(), b"val1".to_vec());
        cache.put(b"key2".to_vec(), b"val2".to_vec());
        assert!(cache.current_bytes() <= 100);
        assert_eq!(cache.get(&b"key1".to_vec()), Some(&b"val1".to_vec()));
    }

    #[test]
    fn bounded_evicts_when_over_budget() {
        // max_bytes = 16 means we can hold two entries of 4+4=8 bytes each.
        let mut cache = make_bounded(16, 100);
        cache.put(b"key1".to_vec(), b"val1".to_vec()); // 8 bytes
        cache.put(b"key2".to_vec(), b"val2".to_vec()); // 8 bytes — total 16
        assert_eq!(cache.current_bytes(), 16);

        // Adding a third entry (8 bytes) should evict the oldest (key1).
        cache.put(b"key3".to_vec(), b"val3".to_vec());
        assert!(cache.current_bytes() <= 16);
        assert!(cache.get(&b"key1".to_vec()).is_none());
    }

    #[test]
    fn bounded_remove_updates_bytes() {
        let mut cache = make_bounded(100, 10);
        cache.put(b"hello".to_vec(), b"world".to_vec());
        let before = cache.current_bytes();
        cache.remove(&b"hello".to_vec());
        assert_eq!(cache.current_bytes(), before - 10); // 5 + 5 = 10 bytes freed
    }

    #[test]
    fn bounded_clear_resets_bytes() {
        let mut cache = make_bounded(100, 10);
        cache.put(b"a".to_vec(), b"b".to_vec());
        cache.clear();
        assert_eq!(cache.current_bytes(), 0);
    }

    #[test]
    fn bounded_update_existing_key() {
        let mut cache = make_bounded(100, 10);
        cache.put(b"key".to_vec(), b"short".to_vec()); // 3+5=8 bytes
        cache.put(b"key".to_vec(), b"longer_value".to_vec()); // 3+12=15 bytes
        assert!(cache.current_bytes() <= 100);
        assert_eq!(cache.get(&b"key".to_vec()), Some(&b"longer_value".to_vec()));
    }
}

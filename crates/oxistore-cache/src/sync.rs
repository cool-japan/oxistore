//! Thread-safe cache wrapper backed by a [`std::sync::Mutex`].
//!
//! [`SyncCache`] wraps any [`Cache`] implementation and provides a
//! `Send + Sync` interface suitable for sharing between threads.  The
//! internal mutex is a standard library `Mutex`, so poisoning is reported as
//! a panic — a poisoned mutex indicates a bug (panic) in another thread, not
//! a recoverable error.
//!
//! # Example
//!
//! ```rust
//! use oxistore_cache::{LruCache, SyncCache};
//! use std::sync::Arc;
//!
//! let cache = Arc::new(SyncCache::new(LruCache::<i32, i32>::new(100)));
//! cache.put(1, 42);
//! assert_eq!(cache.get(&1), Some(42));
//! ```

use std::sync::Mutex;

use crate::Cache;

/// A thread-safe wrapper around any [`Cache`] implementation.
///
/// Wraps `C` (which implements `Cache<K, V>`) in a [`Mutex`], providing
/// `Send + Sync` access from multiple threads.  All methods acquire the
/// mutex for their duration.
///
/// # Panics
///
/// Every method panics if the inner mutex is poisoned, which happens when
/// another thread panicked while holding the lock.
pub struct SyncCache<K, V, C: Cache<K, V>> {
    inner: Mutex<C>,
    _marker: std::marker::PhantomData<(K, V)>,
}

impl<K, V, C: Cache<K, V>> SyncCache<K, V, C> {
    /// Wrap `cache` in a `SyncCache`.
    pub fn new(cache: C) -> Self {
        Self {
            inner: Mutex::new(cache),
            _marker: std::marker::PhantomData,
        }
    }

    /// Look up `key`, cloning and returning the value if present.
    ///
    /// This acquires the lock, calls [`Cache::get`] (which may update
    /// recency), and clones the result so the lock can be released.
    pub fn get(&self, key: &K) -> Option<V>
    where
        V: Clone,
    {
        self.inner.lock().expect("mutex poisoned").get(key).cloned()
    }

    /// Insert or update `key` -> `value`.
    ///
    /// Returns the evicted value (if any) per the underlying cache policy.
    pub fn put(&self, key: K, value: V) -> Option<V> {
        self.inner.lock().expect("mutex poisoned").put(key, value)
    }

    /// Remove the entry for `key`, returning its value if present.
    pub fn remove(&self, key: &K) -> Option<V> {
        self.inner.lock().expect("mutex poisoned").remove(key)
    }

    /// Return the number of live entries in the cache.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("mutex poisoned").len()
    }

    /// Return `true` if the cache holds no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove all entries from the cache.
    pub fn clear(&self) {
        self.inner.lock().expect("mutex poisoned").clear();
    }
}

#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore-core` — Pure Rust storage primitives for OxiStore.
//!
//! This crate provides the foundational traits and error types shared across
//! all OxiStore backends. It is intentionally dependency-free.
//!
//! # Key Traits
//!
//! - [`KvStore`] — key-value store with reads, writes, range scans, transactions, and snapshots.
//! - [`KvTxn`] — explicit write transaction (commit / rollback).
//! - [`KvSnapshot`] — point-in-time read-only view.
//! - [`ColumnarStore`] — stub for M2+ columnar storage.
//! - [`BlobStore`] — stub for M4+ blob storage.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Typed KV adapter with configurable codec (available with `serde-typed` feature).
#[cfg(feature = "serde-typed")]
pub mod typed;

#[cfg(feature = "serde-typed")]
pub use typed::{JsonCodec, TypedCodec, TypedKvError, TypedKvStore};

/// Errors that can be returned by any OxiStore backend.
#[derive(Debug, Clone)]
pub enum StoreError {
    /// An I/O error occurred at the file-system level.
    Io(Arc<std::io::Error>),
    /// The database file is corrupt or in an unrecognized format.
    Corruption(String),
    /// The requested key was not found (used when absence is treated as error).
    NotFound,
    /// A key was inserted but already exists (reserved for unique-insert APIs).
    AlreadyExists,
    /// A write transaction conflicted with a concurrent transaction and must be retried.
    TxnConflict,
    /// The store is open in read-only mode and does not accept writes.
    ReadOnly,
    /// An operation timed out.
    Timeout,
    /// A bounded store or cache has exceeded its capacity limit.
    CapacityExceeded,
    /// A compare-and-swap operation failed because the expected value did not
    /// match the current stored value.
    CasMismatch,
    /// The requested key was not found in the store.
    KeyNotFound,
    /// The operation is not supported by this backend or configuration.
    Unsupported(String),
    /// Any other backend-specific error.
    Other(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "I/O error: {e}"),
            StoreError::Corruption(s) => write!(f, "corruption: {s}"),
            StoreError::NotFound => write!(f, "not found"),
            StoreError::AlreadyExists => write!(f, "already exists"),
            StoreError::TxnConflict => write!(f, "transaction conflict"),
            StoreError::ReadOnly => write!(f, "store is read-only"),
            StoreError::Timeout => write!(f, "operation timed out"),
            StoreError::CapacityExceeded => write!(f, "capacity exceeded"),
            StoreError::CasMismatch => write!(f, "compare-and-swap mismatch"),
            StoreError::KeyNotFound => write!(f, "key not found"),
            StoreError::Unsupported(s) => write!(f, "unsupported: {s}"),
            StoreError::Other(s) => write!(f, "error: {s}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(Arc::new(e))
    }
}

impl From<String> for StoreError {
    fn from(s: String) -> Self {
        StoreError::Other(s)
    }
}

/// Compute the exclusive upper-bound key for a prefix scan.
///
/// Given a prefix like `b"foo"`, returns `Some(b"fop")` — the first key
/// that would sort after all keys sharing the prefix.
///
/// Returns `None` if the prefix is empty or consists entirely of `0xFF`
/// bytes (i.e. every key in the store matches the prefix).
///
/// # Examples
///
/// ```
/// use oxistore_core::prefix_upper_bound;
///
/// assert_eq!(prefix_upper_bound(b"foo"), Some(b"fop".to_vec()));
/// assert_eq!(prefix_upper_bound(b"ab\xff"), Some(b"ac".to_vec()));
/// assert_eq!(prefix_upper_bound(b"\xff\xff"), None);
/// assert_eq!(prefix_upper_bound(b""), None);
/// ```
pub fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    if prefix.is_empty() {
        return None;
    }
    // Walk backwards to find the last byte that is not 0xFF.
    let mut upper = prefix.to_vec();
    while let Some(&last) = upper.last() {
        if last == 0xFF {
            upper.pop();
        } else {
            // Increment the last non-0xFF byte.
            if let Some(b) = upper.last_mut() {
                *b += 1;
            }
            return Some(upper);
        }
    }
    // All bytes were 0xFF — no upper bound.
    None
}

/// Encode a TTL as an expiry unix-epoch-milliseconds `u64`.
///
/// Adds `ttl` to the current [`std::time::SystemTime`] and returns the
/// resulting point in time as milliseconds since the Unix epoch.
///
/// # Errors
///
/// Returns [`StoreError::Other`] if the system clock is before the Unix epoch.
pub fn expiry_epoch_millis(ttl: Duration) -> Result<u64, StoreError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| StoreError::Other(e.to_string()))
        .map(|d| d.checked_add(ttl).unwrap_or(d).as_millis() as u64)
}

/// Return `true` if an epoch-milliseconds timestamp is in the past.
///
/// A timestamp is considered expired when the current time equals or exceeds
/// `expiry_millis`.
#[must_use]
pub fn is_expired(expiry_millis: u64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    expiry_millis <= now
}

/// Core key-value store trait.
///
/// All backend implementations (`redb`, `sled`, ...) implement this trait so that
/// callers can depend only on `oxistore-core` and swap backends via the facade.
///
/// # Thread Safety
///
/// Implementations are required to be `Send + Sync`; interior mutability
/// (e.g. via `Mutex`) is the backend's responsibility.
pub trait KvStore: Send + Sync {
    /// Retrieve the value associated with `key`, or `None` if it is absent.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;

    /// Insert or overwrite a key-value pair.
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError>;

    /// Remove a key.  No-op if the key is absent.
    fn delete(&self, key: &[u8]) -> Result<(), StoreError>;

    /// Retrieve values for multiple keys in a single call.
    ///
    /// Returns a `Vec` of `Option<Vec<u8>>` in the same order as `keys`.
    /// The default implementation calls [`KvStore::get`] for each key
    /// individually; backends with batch-read support should override for
    /// better performance.
    fn get_many(&self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, StoreError> {
        keys.iter().map(|k| self.get(k)).collect()
    }

    /// Retrieve a value as a [`std::borrow::Cow`], avoiding a clone when the
    /// backend can return a borrowed slice.
    ///
    /// The default implementation calls [`KvStore::get`] and wraps the owned
    /// `Vec<u8>` in `Cow::Owned`.  Backends that can return zero-copy
    /// references should override this method.
    fn get_ref<'a>(&'a self, key: &[u8]) -> Result<Option<std::borrow::Cow<'a, [u8]>>, StoreError> {
        self.get(key).map(|opt| opt.map(std::borrow::Cow::Owned))
    }

    /// Return `true` if `key` is present in the store.
    ///
    /// Default implementation delegates to [`KvStore::get`].
    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        Ok(self.get(key)?.is_some())
    }

    /// Return all key-value pairs whose keys fall within `[lo, hi)`,
    /// in ascending key order.
    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError>;

    /// Return all key-value pairs whose keys fall within `[lo, hi)`,
    /// in **descending** key order.
    ///
    /// The default implementation delegates to [`KvStore::range`], collects the
    /// results, and reverses the resulting `Vec`.  Backends that support native
    /// reverse iteration should override for better performance.
    fn range_rev<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let items: Vec<RangeItem> = self.range(lo, hi)?.collect();
        Ok(Box::new(items.into_iter().rev()))
    }

    /// Iterate all key-value pairs sharing the given `prefix`, in ascending
    /// key order.
    ///
    /// The default implementation computes the exclusive upper bound from the
    /// prefix and delegates to [`KvStore::range`].  When the prefix is empty,
    /// the full store is scanned via [`KvStore::iter`].  When the prefix
    /// consists entirely of `0xFF` bytes (no upper bound exists), the result
    /// is obtained via [`KvStore::iter`] filtered to keys that start with the
    /// prefix.
    fn prefix_scan<'a>(&'a self, prefix: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        if prefix.is_empty() {
            return self.iter();
        }
        match prefix_upper_bound(prefix) {
            Some(hi) => self.range(prefix, &hi),
            None => {
                // All-0xFF prefix: no upper bound can be computed.
                // Collect from iter() and filter to keys that start with the prefix.
                let prefix_owned = prefix.to_vec();
                let items: Vec<RangeItem> = self
                    .iter()?
                    .filter(|r| {
                        r.as_ref()
                            .map(|(k, _)| k.starts_with(&prefix_owned))
                            .unwrap_or(true) // propagate errors
                    })
                    .collect();
                Ok(Box::new(items.into_iter()))
            }
        }
    }

    /// Insert multiple key-value pairs atomically in a single batch.
    ///
    /// The default implementation opens a transaction, inserts all pairs,
    /// and commits.  Backends may override for better performance.
    fn batch_write(&self, pairs: &[(&[u8], &[u8])]) -> Result<(), StoreError> {
        let mut txn = self.transaction()?;
        for &(k, v) in pairs {
            txn.put(k, v)?;
        }
        txn.commit()
    }

    /// Delete multiple keys atomically in a single batch.
    ///
    /// The default implementation opens a transaction, deletes all keys,
    /// and commits.  Backends may override for better performance.
    fn batch_delete(&self, keys: &[&[u8]]) -> Result<(), StoreError> {
        let mut txn = self.transaction()?;
        for &k in keys {
            txn.delete(k)?;
        }
        txn.commit()
    }

    /// Return the total number of keys in the store.
    ///
    /// The default implementation performs a full iteration and counts entries.
    /// Backends that maintain key counts natively should override for O(1).
    fn count(&self) -> Result<u64, StoreError> {
        let mut n = 0u64;
        for item in self.iter()? {
            let _ = item?;
            n += 1;
        }
        Ok(n)
    }

    /// Return the approximate byte size of the store on disk.
    ///
    /// The default implementation returns 0 (unknown).  Backends should
    /// override if they can compute the on-disk size cheaply.
    fn size_on_disk(&self) -> Result<u64, StoreError> {
        Ok(0)
    }

    /// Iterate all key-value pairs in the store in ascending key order.
    ///
    /// This is a required method -- all backend implementations must provide
    /// a full-store iteration.
    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError>;

    /// Iterate all keys (without loading values) in ascending order.
    ///
    /// The default implementation wraps [`KvStore::iter`] and discards values.
    /// Backends that can iterate keys without reading values should override.
    fn keys<'a>(&'a self) -> Result<KeysIter<'a>, StoreError> {
        let it = self.iter()?;
        Ok(Box::new(it.map(|r| r.map(|(k, _v)| k))))
    }

    /// Atomic compare-and-swap: if the current value for `key` equals
    /// `expected`, replace it with `new_value` and return `Ok(true)`.
    /// If the current value does not match `expected`, return `Ok(false)`.
    ///
    /// `expected` is `None` for "key must not exist", `Some(v)` for
    /// "key must hold value v".
    ///
    /// The default implementation uses a transaction for atomicity.
    fn compare_and_swap(
        &self,
        key: &[u8],
        expected: Option<&[u8]>,
        new_value: &[u8],
    ) -> Result<bool, StoreError> {
        let mut txn = self.transaction()?;
        let current = txn.get(key)?;
        let matches = match (current.as_deref(), expected) {
            (None, None) => true,
            (Some(cur), Some(exp)) => cur == exp,
            _ => false,
        };
        if matches {
            txn.put(key, new_value)?;
            txn.commit()?;
            Ok(true)
        } else {
            txn.rollback()?;
            Ok(false)
        }
    }

    /// Insert a key-value pair with a time-to-live.  After `ttl` has elapsed,
    /// the key is treated as absent (expired).
    ///
    /// Backends that support native TTL should override this method.
    /// The default implementation returns [`StoreError::Unsupported`].
    fn put_with_ttl(&self, _key: &[u8], _value: &[u8], _ttl: Duration) -> Result<(), StoreError> {
        Err(StoreError::Unsupported("TTL not supported".to_string()))
    }

    /// Set a TTL on an existing key.  The key must already exist.
    ///
    /// After `ttl` has elapsed the key is treated as absent.
    /// The default implementation returns [`StoreError::Unsupported`].
    fn expire(&self, _key: &[u8], _ttl: Duration) -> Result<(), StoreError> {
        Err(StoreError::Unsupported("TTL not supported".to_string()))
    }

    /// Return the remaining TTL for a key.
    ///
    /// Returns `Ok(None)` if the key exists but has no TTL attached.
    /// Returns `Err(StoreError::KeyNotFound)` if the key does not exist.
    /// Returns `Err(StoreError::Unsupported)` by default.
    fn ttl(&self, _key: &[u8]) -> Result<Option<Duration>, StoreError> {
        Err(StoreError::Unsupported("TTL not supported".to_string()))
    }

    /// Remove the TTL from a key, making it persistent.
    ///
    /// Returns `Ok(true)` if the key existed and its TTL was removed,
    /// `Ok(false)` if the key exists but had no TTL.
    /// Returns `Err(StoreError::KeyNotFound)` if the key does not exist.
    /// The default implementation returns [`StoreError::Unsupported`].
    fn persist(&self, _key: &[u8]) -> Result<bool, StoreError> {
        Err(StoreError::Unsupported("TTL not supported".to_string()))
    }

    /// Scan and delete all expired keys eagerly.
    ///
    /// Returns the count of keys that were deleted.
    /// The default implementation is a no-op returning `Ok(0)`.
    fn purge_expired(&self) -> Result<u64, StoreError> {
        Ok(0)
    }

    /// Trigger manual compaction on backends that support it.
    ///
    /// The default implementation is a no-op.
    fn compact(&self) -> Result<(), StoreError> {
        Ok(())
    }

    /// Create a point-in-time backup to the given path.
    ///
    /// The default implementation returns an error indicating backup is
    /// not supported.  Backends should override if they support backup.
    fn backup(&self, _path: &Path) -> Result<(), StoreError> {
        Err(StoreError::Other(
            "backup not supported for this backend".to_string(),
        ))
    }

    /// Restore from a backup at the given path.
    ///
    /// The default implementation returns an error.  Backends should
    /// override if they support restore.
    fn restore(&self, _path: &Path) -> Result<(), StoreError> {
        Err(StoreError::Other(
            "restore not supported for this backend".to_string(),
        ))
    }

    /// Begin an explicit write transaction.
    ///
    /// Changes made through [`KvTxn`] are only visible after [`KvTxn::commit`].
    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError>;

    /// Capture a point-in-time read-only snapshot of the store.
    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError>;

    /// Ensure all committed data has been written to durable storage.
    ///
    /// The exact semantics depend on the backend; for backends that auto-flush
    /// (e.g. redb commits), this is a no-op or an advisory hint.
    fn flush(&self) -> Result<(), StoreError>;
}

/// An explicit write transaction obtained from [`KvStore::transaction`].
///
/// All mutations made through `KvTxn` are buffered until [`KvTxn::commit`] is
/// called.  Dropping without committing has the same effect as
/// [`KvTxn::rollback`].
pub trait KvTxn {
    /// Read a value from the store within this transaction's view.
    ///
    /// Implementations that support read-your-writes should return buffered
    /// writes that have not yet been committed.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;

    /// Stage a key-value insertion in the transaction.
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError>;

    /// Stage a key deletion in the transaction.
    fn delete(&mut self, key: &[u8]) -> Result<(), StoreError>;

    /// Check whether `key` exists within this transaction's view.
    ///
    /// Default implementation delegates to [`KvTxn::get`].
    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        Ok(self.get(key)?.is_some())
    }

    /// Range scan within the transaction's view.
    ///
    /// Implementations supporting read-your-writes should merge buffered
    /// writes with committed data.  The default implementation returns an
    /// error indicating range is not supported within transactions.
    fn range<'a>(&'a self, _lo: &[u8], _hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        Err(StoreError::Other(
            "range not supported within this transaction type".to_string(),
        ))
    }

    /// Commit all staged changes atomically.
    fn commit(self: Box<Self>) -> Result<(), StoreError>;

    /// Discard all staged changes.
    fn rollback(self: Box<Self>) -> Result<(), StoreError>;
}

/// A point-in-time read-only view of the store obtained from [`KvStore::snapshot`].
pub trait KvSnapshot {
    /// Read a value from the snapshot.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;

    /// Return all key-value pairs whose keys fall within `[lo, hi)`,
    /// in ascending key order.
    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError>;

    /// Return all key-value pairs sharing the given `prefix`, in ascending
    /// key order.
    ///
    /// Default implementation uses [`prefix_upper_bound`] and delegates to
    /// [`KvSnapshot::range`].
    fn prefix_scan<'a>(&'a self, prefix: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        match prefix_upper_bound(prefix) {
            Some(hi) => self.range(prefix, &hi),
            None => {
                // No upper bound — scan everything.
                self.range(&[], &[])
            }
        }
    }

    /// Check whether `key` exists in the snapshot.
    ///
    /// Default implementation delegates to [`KvSnapshot::get`].
    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        Ok(self.get(key)?.is_some())
    }
}

/// Stub trait for M2+ columnar store — defined here so facade re-exports remain stable.
pub trait ColumnarStore: Send + Sync {}

/// Stub trait for M4+ blob store — defined here so facade re-exports remain stable.
pub trait BlobStore: Send + Sync {}

/// Convenience alias: a heap-allocated [`KvStore`] with `'static` lifetime.
///
/// Returned by `oxistore::open`.
pub type BoxKvStore = Box<dyn KvStore>;

/// A single item produced by a range scan: a `(key, value)` pair or an error.
pub type RangeItem = Result<(Vec<u8>, Vec<u8>), StoreError>;

/// A boxed iterator over [`RangeItem`]s with a given lifetime.
pub type RangeIter<'a> = Box<dyn Iterator<Item = RangeItem> + 'a>;

/// A boxed iterator over keys (without values) with a given lifetime.
///
/// Used by [`KvStore::keys`].
pub type KeysIter<'a> = Box<dyn Iterator<Item = Result<Vec<u8>, StoreError>> + 'a>;

/// Backend-agnostic configuration for opening a store.
///
/// Each backend maps the fields it supports and ignores the rest.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// Block cache size in bytes (backend-specific interpretation).
    pub cache_size_bytes: Option<u64>,
    /// Whether to sync writes to disk on every commit.
    pub sync_writes: bool,
    /// Whether to open the store in read-only mode.
    pub read_only: bool,
}

impl Default for StoreConfig {
    fn default() -> Self {
        StoreConfig {
            cache_size_bytes: None,
            sync_writes: true,
            read_only: false,
        }
    }
}

/// Runtime statistics for a store (reads, writes, cache hits, etc.).
#[derive(Debug, Clone, Default)]
pub struct StoreMetrics {
    /// Total number of `get` calls.
    pub reads: u64,
    /// Total number of `put` calls.
    pub writes: u64,
    /// Total number of `delete` calls.
    pub deletes: u64,
    /// Total bytes read.
    pub bytes_read: u64,
    /// Total bytes written.
    pub bytes_written: u64,
    /// Cache hit count (if a cache layer is in use).
    pub cache_hits: u64,
    /// Cache miss count (if a cache layer is in use).
    pub cache_misses: u64,
}

impl StoreMetrics {
    /// Compute the cache hit rate as a fraction (0.0 to 1.0).
    ///
    /// Returns 0.0 if no cache lookups have been performed.
    #[must_use]
    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }
}

/// Ensure the path's parent directory exists, creating it if necessary.
///
/// This helper is used by backend `open` implementations to avoid confusing
/// "file not found" errors when the parent directory does not exist.
pub fn ensure_parent_dir(path: &Path) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_upper_bound_basic() {
        assert_eq!(prefix_upper_bound(b"foo"), Some(b"fop".to_vec()));
    }

    #[test]
    fn prefix_upper_bound_trailing_ff() {
        assert_eq!(prefix_upper_bound(b"ab\xff"), Some(b"ac".to_vec()));
    }

    #[test]
    fn prefix_upper_bound_all_ff() {
        assert_eq!(prefix_upper_bound(b"\xff\xff"), None);
    }

    #[test]
    fn prefix_upper_bound_empty() {
        assert_eq!(prefix_upper_bound(b""), None);
    }

    #[test]
    fn prefix_upper_bound_single_byte() {
        assert_eq!(prefix_upper_bound(b"a"), Some(b"b".to_vec()));
    }

    #[test]
    fn store_error_display() {
        assert_eq!(format!("{}", StoreError::NotFound), "not found");
        assert_eq!(format!("{}", StoreError::ReadOnly), "store is read-only");
        assert_eq!(format!("{}", StoreError::Timeout), "operation timed out");
        assert_eq!(
            format!("{}", StoreError::CapacityExceeded),
            "capacity exceeded"
        );
        assert_eq!(
            format!("{}", StoreError::CasMismatch),
            "compare-and-swap mismatch"
        );
    }

    #[test]
    fn store_error_from_string() {
        let err: StoreError = "test error".to_string().into();
        assert_eq!(format!("{err}"), "error: test error");
    }

    #[test]
    fn store_config_default() {
        let cfg = StoreConfig::default();
        assert!(cfg.cache_size_bytes.is_none());
        assert!(cfg.sync_writes);
        assert!(!cfg.read_only);
    }

    #[test]
    fn store_metrics_hit_rate() {
        let m = StoreMetrics {
            cache_hits: 80,
            cache_misses: 20,
            ..StoreMetrics::default()
        };
        assert!((m.cache_hit_rate() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn store_metrics_hit_rate_zero() {
        let m = StoreMetrics::default();
        assert!((m.cache_hit_rate()).abs() < f64::EPSILON);
    }

    // ── core-clone-error ────────────────────────────────────────────────────

    #[test]
    fn store_error_clone_io() {
        let original = StoreError::from(std::io::Error::new(std::io::ErrorKind::NotFound, "test"));
        let cloned = original.clone();
        if let StoreError::Io(arc) = cloned {
            assert_eq!(arc.kind(), std::io::ErrorKind::NotFound);
        } else {
            panic!("expected StoreError::Io after clone");
        }
    }

    #[test]
    fn store_error_clone_non_io_variants() {
        let variants = [
            StoreError::NotFound,
            StoreError::AlreadyExists,
            StoreError::TxnConflict,
            StoreError::ReadOnly,
            StoreError::Timeout,
            StoreError::CapacityExceeded,
            StoreError::CasMismatch,
            StoreError::KeyNotFound,
            StoreError::Corruption("bad".to_string()),
            StoreError::Unsupported("nope".to_string()),
            StoreError::Other("misc".to_string()),
        ];
        for v in &variants {
            let _ = v.clone(); // must not panic
        }
    }

    // ── core-range-rev ──────────────────────────────────────────────────────

    /// Minimal in-memory `KvStore` for unit-testing default-method behaviour.
    struct MemKv(std::sync::Mutex<std::collections::BTreeMap<Vec<u8>, Vec<u8>>>);

    impl MemKv {
        fn new() -> Self {
            MemKv(std::sync::Mutex::new(std::collections::BTreeMap::new()))
        }
    }

    impl KvStore for MemKv {
        fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }

        fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
            self.0.lock().unwrap().insert(key.to_vec(), value.to_vec());
            Ok(())
        }

        fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }

        fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
            use std::ops::Bound;
            let map = self.0.lock().unwrap();
            let pairs: Vec<RangeItem> = map
                .range((Bound::Included(lo.to_vec()), Bound::Excluded(hi.to_vec())))
                .map(|(k, v)| Ok((k.clone(), v.clone())))
                .collect();
            Ok(Box::new(pairs.into_iter()))
        }

        fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
            let map = self.0.lock().unwrap();
            let pairs: Vec<RangeItem> = map
                .iter()
                .map(|(k, v)| Ok((k.clone(), v.clone())))
                .collect();
            Ok(Box::new(pairs.into_iter()))
        }

        fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
            Err(StoreError::Unsupported("no txn in MemKv".to_string()))
        }

        fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
            Err(StoreError::Unsupported("no snapshot in MemKv".to_string()))
        }

        fn flush(&self) -> Result<(), StoreError> {
            Ok(())
        }
    }

    #[test]
    fn range_rev_descending_order() {
        let store = MemKv::new();
        store.put(b"a", b"1").unwrap();
        store.put(b"b", b"2").unwrap();
        store.put(b"c", b"3").unwrap();
        store.put(b"d", b"4").unwrap();

        // range_rev over [a, e) should yield d, c, b, a in that order.
        let items: Vec<(Vec<u8>, Vec<u8>)> = store
            .range_rev(b"a", b"e")
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let keys: Vec<&[u8]> = items.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(keys, vec![b"d", b"c", b"b", b"a"]);
    }

    #[test]
    fn range_rev_empty_range() {
        let store = MemKv::new();
        store.put(b"x", b"v").unwrap();

        // [z, z) is empty — range_rev should return an empty iterator.
        let items: Vec<_> = store.range_rev(b"z", b"z").unwrap().collect();
        assert!(items.is_empty());
    }

    // ── ensure_parent_dir edge cases ────────────────────────────────────────

    #[test]
    fn ensure_parent_dir_empty_path() {
        // Empty path has no parent — should succeed (no-op)
        let result = ensure_parent_dir(std::path::Path::new("some_file.db"));
        assert!(result.is_ok());
    }

    #[test]
    fn ensure_parent_dir_nested() {
        use std::process;
        let tmp = std::env::temp_dir().join(format!("oxistore_ensure_parent_{}", process::id()));
        let deep = tmp.join("a").join("b").join("file.db");
        let result = ensure_parent_dir(&deep);
        assert!(result.is_ok());
        assert!(deep.parent().expect("has parent").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_parent_dir_already_exists() {
        let tmp = std::env::temp_dir();
        let path = tmp.join("existing_check.db");
        // tmp already exists — should succeed without creating anything new
        let result = ensure_parent_dir(&path);
        assert!(result.is_ok());
    }

    // ── StoreError::from(io::Error) variants ───────────────────────────────

    #[test]
    fn store_error_from_io_error_variants() {
        use std::io;
        let kinds = [
            io::ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::AlreadyExists,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::TimedOut,
        ];
        for kind in kinds {
            let io_err = io::Error::new(kind, "test error");
            let store_err: StoreError = io_err.into();
            match &store_err {
                StoreError::Io(arc) => assert_eq!(arc.kind(), kind),
                other => panic!("expected StoreError::Io, got {other:?}"),
            }
        }
    }
}

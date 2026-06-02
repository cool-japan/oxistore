#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore-kv-sled` — [sled](https://crates.io/crates/sled)-backed [`KvStore`] implementation.
//!
//! This crate provides [`SledStore`], a key-value store built on top of the
//! [sled] embedded database.  It implements the [`oxistore_core::KvStore`]
//! trait so it can be used through the `oxistore` facade or directly.
//!
//! # Transaction model
//!
//! sled 0.34 uses a closure-based transaction API.  This crate provides a
//! buffered [`SledTxn`] that collects operations and applies them atomically
//! inside a sled transaction on [`KvTxn::commit`].  Reads within the
//! transaction support **read-your-writes**: buffered puts and deletes are
//! visible immediately via the local overlay.
//!
//! # Snapshot model
//!
//! sled 0.34 does not expose a snapshot API.  [`KvStore::snapshot`] therefore
//! materialises the entire current tree into a `BTreeMap` at call time.
//!
//! # Example
//!
//! ```no_run
//! use oxistore_kv_sled::SledStore;
//! use oxistore_core::KvStore;
//!
//! let store = SledStore::open("/tmp/my-sled").expect("open failed");
//! store.put(b"hello", b"world").expect("put failed");
//! let val = store.get(b"hello").expect("get failed");
//! assert_eq!(val.as_deref(), Some(b"world".as_ref()));
//! ```

use std::collections::BTreeMap;

use oxistore_core::{
    expiry_epoch_millis, is_expired, KeysIter, KvSnapshot, KvStore, KvTxn, RangeIter, StoreError,
};

// ------------------------------------------------------------------
// SledStore
// ------------------------------------------------------------------

/// Encode an expiry timestamp as 8 little-endian bytes.
fn encode_expiry(millis: u64) -> [u8; 8] {
    millis.to_le_bytes()
}

/// Decode an 8-byte little-endian expiry timestamp.
fn decode_expiry(b: &[u8]) -> Option<u64> {
    b.try_into().ok().map(u64::from_le_bytes)
}

/// A [`KvStore`] backed by [sled](https://crates.io/crates/sled).
///
/// Primary data is stored in the `"default"` sled tree.  TTL expiry
/// timestamps are stored in a separate `"__ttl__"` tree.
#[derive(Clone)]
pub struct SledStore {
    db: sled::Db,
    tree: sled::Tree,
    ttl_tree: sled::Tree,
}

impl SledStore {
    /// Open (or create) a sled database at `path`.
    ///
    /// If the directory does not exist it is created automatically.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        oxistore_core::ensure_parent_dir(path)?;
        let db = sled::open(path).map_err(|e| StoreError::Other(e.to_string()))?;
        let tree = db
            .open_tree("default")
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let ttl_tree = db
            .open_tree(b"__ttl__")
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(Self { db, tree, ttl_tree })
    }

    /// Open an ephemeral (temporary) sled database that is deleted on drop.
    ///
    /// Useful for tests and short-lived workloads.
    pub fn open_temporary() -> Result<Self, StoreError> {
        let db = sled::Config::new()
            .temporary(true)
            .open()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let tree = db
            .open_tree("default")
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let ttl_tree = db
            .open_tree(b"__ttl__")
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(Self { db, tree, ttl_tree })
    }

    // ------------------------------------------------------------------
    // Extended sled-specific APIs
    // ------------------------------------------------------------------

    /// Set a merge operator on the default tree.
    ///
    /// The merge function receives `(key, existing_value_or_none, new_bytes)`
    /// and returns the merged value, or `None` to delete the key.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use oxistore_kv_sled::SledStore;
    /// let store = SledStore::open_temporary().unwrap();
    /// store.set_merge_operator(|_key, old, new_bytes| {
    ///     let mut v = old.map(|o| o.to_vec()).unwrap_or_default();
    ///     v.extend_from_slice(new_bytes);
    ///     Some(v)
    /// });
    /// store.merge(b"k", b"hello").unwrap();
    /// store.merge(b"k", b" world").unwrap();
    /// assert_eq!(store.get(b"k").unwrap(), Some(b"hello world".to_vec()));
    /// ```
    pub fn set_merge_operator(
        &self,
        merge_operator: impl Fn(&[u8], Option<&[u8]>, &[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) {
        self.tree.set_merge_operator(merge_operator);
    }

    /// Merge `value` into the entry at `key` using the configured merge operator.
    ///
    /// Returns an error if no merge operator has been configured on this tree.
    pub fn merge(&self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Result<(), StoreError> {
        self.tree
            .merge(key, value)
            .map(|_| ())
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    /// Subscribe to changes on keys sharing the given `prefix`.
    ///
    /// Returns a [`sled::Subscriber`] that yields [`sled::Event`]s whenever
    /// a matching key is inserted, updated, or removed.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use oxistore_kv_sled::SledStore;
    /// # use oxistore_core::KvStore;
    /// let store = SledStore::open_temporary().unwrap();
    /// let mut sub = store.watch_prefix(b"user:");
    /// // In another thread:
    /// store.put(b"user:1", b"alice").unwrap();
    /// // The subscriber yields a sled::Event for the insertion.
    /// let _event = sub.next();
    /// ```
    pub fn watch_prefix(&self, prefix: impl AsRef<[u8]>) -> sled::Subscriber {
        self.tree.watch_prefix(prefix)
    }

    /// Open or create a named tree in the underlying sled database.
    ///
    /// Named trees are logically isolated: keys written to one tree are
    /// invisible in any other tree (including `"default"`).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use oxistore_kv_sled::SledStore;
    /// let store = SledStore::open_temporary().unwrap();
    /// let alpha = store.open_tree("alpha").unwrap();
    /// let beta = store.open_tree("beta").unwrap();
    /// alpha.insert(b"key", b"from-alpha").unwrap();
    /// assert!(beta.get(b"key").unwrap().is_none());
    /// ```
    pub fn open_tree(&self, name: impl AsRef<[u8]>) -> Result<sled::Tree, StoreError> {
        self.db
            .open_tree(name)
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    /// Force all pending writes to disk (WAL + data flush).
    ///
    /// This calls `sled::Db::flush` and blocks until all data written so far
    /// is durably persisted to the underlying storage.  Unlike the
    /// [`KvStore::flush`] implementation (which is a best-effort, potentially
    /// non-blocking flush), `flush_sync` guarantees that the call returns only
    /// after the OS confirms the write is durable.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the underlying flush fails.
    pub fn flush_sync(&self) -> Result<(), StoreError> {
        self.db
            .flush()
            .map(|_| ())
            .map_err(|e| StoreError::Io(std::sync::Arc::new(std::io::Error::other(e.to_string()))))
    }
}

impl KvStore for SledStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        // Check for TTL expiry before returning the value.
        if let Some(expiry_bytes) = self
            .ttl_tree
            .get(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
        {
            if let Some(expiry_millis) = decode_expiry(&expiry_bytes) {
                if is_expired(expiry_millis) {
                    // Lazy eviction: remove from both trees.
                    self.tree
                        .remove(key)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    self.ttl_tree
                        .remove(key)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    return Ok(None);
                }
            }
        }
        self.tree
            .get(key)
            .map(|opt| opt.map(|iv| iv.to_vec()))
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.tree
            .insert(key, value)
            .map(|_| ())
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.tree
            .remove(key)
            .map(|_| ())
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let lo_owned = lo.to_vec();
        let hi_owned = hi.to_vec();
        let pairs: Vec<oxistore_core::RangeItem> = self
            .tree
            .range(lo_owned..hi_owned)
            .map(|item| {
                item.map(|(k, v)| (k.to_vec(), v.to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn prefix_scan<'a>(&'a self, prefix: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let prefix_owned = prefix.to_vec();
        let pairs: Vec<oxistore_core::RangeItem> = self
            .tree
            .scan_prefix(&prefix_owned)
            .map(|item| {
                item.map(|(k, v)| (k.to_vec(), v.to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn batch_write(&self, pairs: &[(&[u8], &[u8])]) -> Result<(), StoreError> {
        let mut batch = sled::Batch::default();
        for &(k, v) in pairs {
            batch.insert(k, v);
        }
        self.tree
            .apply_batch(batch)
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn batch_delete(&self, keys: &[&[u8]]) -> Result<(), StoreError> {
        let mut batch = sled::Batch::default();
        for &k in keys {
            batch.remove(k);
        }
        self.tree
            .apply_batch(batch)
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn count(&self) -> Result<u64, StoreError> {
        Ok(self.tree.len() as u64)
    }

    fn size_on_disk(&self) -> Result<u64, StoreError> {
        self.db
            .size_on_disk()
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let pairs: Vec<oxistore_core::RangeItem> = self
            .tree
            .iter()
            .map(|item| {
                item.map(|(k, v)| (k.to_vec(), v.to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn keys<'a>(&'a self) -> Result<KeysIter<'a>, StoreError> {
        let keys: Vec<Result<Vec<u8>, StoreError>> = self
            .tree
            .iter()
            .map(|item| {
                item.map(|(k, _v)| k.to_vec())
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(keys.into_iter()))
    }

    fn compare_and_swap(
        &self,
        key: &[u8],
        expected: Option<&[u8]>,
        new_value: &[u8],
    ) -> Result<bool, StoreError> {
        match self
            .tree
            .compare_and_swap(key, expected, Some(new_value))
            .map_err(|e| StoreError::Other(e.to_string()))?
        {
            Ok(()) => Ok(true),
            Err(_cas_err) => Ok(false),
        }
    }

    fn compact(&self) -> Result<(), StoreError> {
        self.db
            .flush()
            .map(|_| ())
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn backup(&self, dest: &std::path::Path) -> Result<(), StoreError> {
        oxistore_core::ensure_parent_dir(dest)?;
        let export = self.db.export();
        let dest_db = sled::open(dest).map_err(|e| StoreError::Other(e.to_string()))?;
        dest_db.import(export);
        dest_db
            .flush()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }

    fn restore(&self, src: &std::path::Path) -> Result<(), StoreError> {
        let src_db = sled::open(src).map_err(|e| StoreError::Other(e.to_string()))?;
        let export = src_db.export();
        self.db.import(export);
        self.db
            .flush()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Ok(Box::new(SledTxn {
            tree: &self.tree,
            ops: Vec::new(),
            overlay: BTreeMap::new(),
            rolled_back: false,
        }))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        let mut map = std::collections::BTreeMap::new();
        for item in self.tree.iter() {
            let (k, v) = item.map_err(|e| StoreError::Other(e.to_string()))?;
            map.insert(k.to_vec(), v.to_vec());
        }
        Ok(Box::new(SledSnapshot { data: map }))
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.db
            .flush()
            .map(|_| ())
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn put_with_ttl(
        &self,
        key: &[u8],
        value: &[u8],
        ttl: std::time::Duration,
    ) -> Result<(), StoreError> {
        let expiry = expiry_epoch_millis(ttl)?;
        self.tree
            .insert(key, value)
            .map_err(|e| StoreError::Other(e.to_string()))?;
        self.ttl_tree
            .insert(key, encode_expiry(expiry).as_ref())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }

    fn expire(&self, key: &[u8], ttl: std::time::Duration) -> Result<(), StoreError> {
        let exists = self
            .tree
            .contains_key(key)
            .map_err(|e| StoreError::Other(e.to_string()))?;
        if !exists {
            return Err(StoreError::KeyNotFound);
        }
        let expiry = expiry_epoch_millis(ttl)?;
        self.ttl_tree
            .insert(key, encode_expiry(expiry).as_ref())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }

    fn ttl(&self, key: &[u8]) -> Result<Option<std::time::Duration>, StoreError> {
        let data_exists = self
            .tree
            .contains_key(key)
            .map_err(|e| StoreError::Other(e.to_string()))?;
        if !data_exists {
            return Err(StoreError::KeyNotFound);
        }
        match self
            .ttl_tree
            .get(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
        {
            None => Ok(None),
            Some(expiry_bytes) => {
                let expiry_millis = decode_expiry(&expiry_bytes)
                    .ok_or_else(|| StoreError::Other("invalid TTL encoding".to_string()))?;
                if is_expired(expiry_millis) {
                    self.tree
                        .remove(key)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    self.ttl_tree
                        .remove(key)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    Err(StoreError::KeyNotFound)
                } else {
                    let now_millis = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let remaining_millis = expiry_millis.saturating_sub(now_millis);
                    Ok(Some(std::time::Duration::from_millis(remaining_millis)))
                }
            }
        }
    }

    fn persist(&self, key: &[u8]) -> Result<bool, StoreError> {
        let data_exists = self
            .tree
            .contains_key(key)
            .map_err(|e| StoreError::Other(e.to_string()))?;
        if !data_exists {
            return Err(StoreError::KeyNotFound);
        }
        let had_ttl = self
            .ttl_tree
            .remove(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
            .is_some();
        Ok(had_ttl)
    }

    fn purge_expired(&self) -> Result<u64, StoreError> {
        let mut count = 0u64;
        for item in self.ttl_tree.iter() {
            let (key, expiry_bytes) = item.map_err(|e| StoreError::Other(e.to_string()))?;
            if let Some(expiry_millis) = decode_expiry(&expiry_bytes) {
                if is_expired(expiry_millis) {
                    self.tree
                        .remove(&key)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    self.ttl_tree
                        .remove(&key)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

// ------------------------------------------------------------------
// SledTxn — with read-your-writes overlay
// ------------------------------------------------------------------

/// Operation staged in a [`SledTxn`].
enum SledOp {
    /// Insert or overwrite a key.
    Put(Vec<u8>, Vec<u8>),
    /// Delete a key.
    Delete(Vec<u8>),
}

/// Buffered operation within a transaction overlay.
#[derive(Clone)]
enum TxnOp {
    /// Value was inserted/updated.
    Put(Vec<u8>),
    /// Key was deleted.
    Delete,
}

/// A buffered write transaction over a [`sled::Tree`].
///
/// Reads now support **read-your-writes** via a local overlay: buffered
/// puts and deletes are visible immediately within the transaction.
pub struct SledTxn<'a> {
    tree: &'a sled::Tree,
    ops: Vec<SledOp>,
    /// Local overlay for read-your-writes.
    overlay: BTreeMap<Vec<u8>, TxnOp>,
    rolled_back: bool,
}

impl KvTxn for SledTxn<'_> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        // Check the overlay first (read-your-writes).
        if let Some(op) = self.overlay.get(key) {
            return match op {
                TxnOp::Put(v) => Ok(Some(v.clone())),
                TxnOp::Delete => Ok(None),
            };
        }
        // Fall through to committed state.
        self.tree
            .get(key)
            .map(|opt| opt.map(|iv| iv.to_vec()))
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.overlay
            .insert(key.to_vec(), TxnOp::Put(value.to_vec()));
        self.ops.push(SledOp::Put(key.to_vec(), value.to_vec()));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StoreError> {
        self.overlay.insert(key.to_vec(), TxnOp::Delete);
        self.ops.push(SledOp::Delete(key.to_vec()));
        Ok(())
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        Ok(self.get(key)?.is_some())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let lo_owned = lo.to_vec();
        let hi_owned = hi.to_vec();

        // Start with committed data.
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for item in self.tree.range(lo_owned.clone()..hi_owned.clone()) {
            let (k, v) = item.map_err(|e| StoreError::Other(e.to_string()))?;
            merged.insert(k.to_vec(), v.to_vec());
        }

        // Apply overlay.
        for (k, op) in self.overlay.range(lo_owned..hi_owned) {
            match op {
                TxnOp::Put(v) => {
                    merged.insert(k.clone(), v.clone());
                }
                TxnOp::Delete => {
                    merged.remove(k);
                }
            }
        }

        let pairs: Vec<oxistore_core::RangeItem> =
            merged.into_iter().map(|(k, v)| Ok((k, v))).collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn commit(self: Box<Self>) -> Result<(), StoreError> {
        if self.rolled_back {
            return Ok(());
        }
        let ops = self.ops;
        let tree = self.tree;
        tree.transaction(
            |tx| -> sled::transaction::ConflictableTransactionResult<(), ()> {
                for op in &ops {
                    match op {
                        SledOp::Put(k, v) => {
                            tx.insert(k.as_slice(), v.as_slice())?;
                        }
                        SledOp::Delete(k) => {
                            tx.remove(k.as_slice())?;
                        }
                    }
                }
                Ok(())
            },
        )
        .map_err(|e: sled::transaction::TransactionError<()>| match e {
            sled::transaction::TransactionError::Abort(()) => StoreError::TxnConflict,
            sled::transaction::TransactionError::Storage(se) => StoreError::Other(se.to_string()),
        })
    }

    fn rollback(mut self: Box<Self>) -> Result<(), StoreError> {
        self.rolled_back = true;
        Ok(())
    }
}

// ------------------------------------------------------------------
// SledSnapshot
// ------------------------------------------------------------------

// ------------------------------------------------------------------
// SledStoreBuilder
// ------------------------------------------------------------------

/// Builder for [`SledStore`].
///
/// Provides fine-grained control over the underlying sled database:
/// custom cache capacity, flush interval, compression, and temporary
/// (auto-deleted) mode.
///
/// # Example
///
/// ```no_run
/// use oxistore_kv_sled::SledStoreBuilder;
/// use oxistore_core::KvStore;
///
/// let store = SledStoreBuilder::new()
///     .cache_capacity(64 * 1024 * 1024)
///     .use_compression(true)
///     .build("/tmp/my-sled-store")
///     .expect("build failed");
/// store.put(b"hello", b"world").expect("put failed");
/// ```
pub struct SledStoreBuilder {
    cache_capacity: Option<u64>,
    flush_every_ms: Option<u64>,
    use_compression: bool,
    temporary: bool,
}

impl Default for SledStoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SledStoreBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self {
            cache_capacity: None,
            flush_every_ms: None,
            use_compression: false,
            temporary: false,
        }
    }

    /// Set the sled page cache capacity in bytes.
    #[must_use]
    pub fn cache_capacity(mut self, bytes: u64) -> Self {
        self.cache_capacity = Some(bytes);
        self
    }

    /// Set how often (in milliseconds) sled flushes dirty data to disk.
    ///
    /// Pass `None` to disable background flushing (flush on demand only).
    #[must_use]
    pub fn flush_every_ms(mut self, ms: u64) -> Self {
        self.flush_every_ms = Some(ms);
        self
    }

    /// Enable or disable sled's built-in compression.
    #[must_use]
    pub fn use_compression(mut self, enabled: bool) -> Self {
        self.use_compression = enabled;
        self
    }

    /// Mark this database as temporary.
    ///
    /// When `true`, the on-disk storage (if any) is deleted when the
    /// database is dropped.  This is useful for test workloads.
    #[must_use]
    pub fn temporary(mut self, temp: bool) -> Self {
        self.temporary = temp;
        self
    }

    /// Build a [`SledStore`] at the given path (or as a temporary store if
    /// [`Self::temporary`] was set to `true`).
    ///
    /// The directory is created automatically if it does not exist.
    pub fn build(self, path: impl AsRef<std::path::Path>) -> Result<SledStore, StoreError> {
        let mut config = sled::Config::new().path(path.as_ref());
        if let Some(cc) = self.cache_capacity {
            config = config.cache_capacity(cc);
        }
        if let Some(fms) = self.flush_every_ms {
            config = config.flush_every_ms(Some(fms));
        }
        config = config.use_compression(self.use_compression);
        if self.temporary {
            config = config.temporary(true);
        }
        let db = config
            .open()
            .map_err(|e| StoreError::Corruption(e.to_string()))?;
        let tree = db
            .open_tree("default")
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let ttl_tree = db
            .open_tree(b"__ttl__")
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(SledStore { db, tree, ttl_tree })
    }
}

/// A point-in-time snapshot of a sled tree, materialised into a `BTreeMap`.
pub struct SledSnapshot {
    data: std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
}

impl KvSnapshot for SledSnapshot {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.data.get(key).cloned())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let lo_owned = lo.to_vec();
        let hi_owned = hi.to_vec();
        let pairs: Vec<oxistore_core::RangeItem> = self
            .data
            .range(lo_owned..hi_owned)
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }
}

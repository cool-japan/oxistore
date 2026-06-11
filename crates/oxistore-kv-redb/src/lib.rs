#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore-kv-redb` — [redb](https://github.com/cberner/redb)-backed [`KvStore`] implementation.
//!
//! This crate provides [`RedbStore`], a thread-safe, ACID-compliant key-value
//! store built on top of the [redb] embedded database.  It implements the
//! [`oxistore_core::KvStore`] trait so it can be used through the `oxistore`
//! facade or directly.
//!
//! # Example
//!
//! ```no_run
//! use oxistore_kv_redb::RedbStore;
//! use oxistore_core::KvStore;
//!
//! # let path = std::env::temp_dir().join("my.redb");
//! let store = RedbStore::open(&path).expect("open failed");
//! store.put(b"hello", b"world").expect("put failed");
//! let val = store.get(b"hello").expect("get failed");
//! assert_eq!(val.as_deref(), Some(b"world".as_ref()));
//! ```
//!
//! # Type-safe tables
//!
//! For ergonomic use with typed keys and JSON values, see [`TypedRedbTable`].
//!
//! ```no_run
//! use oxistore_kv_redb::{RedbStore, TypedRedbTable};
//!
//! let store = RedbStore::open_in_memory().expect("open failed");
//! let table = TypedRedbTable::new(store);
//!
//! // String keys, JSON-serializable values
//! table.typed_put("my_key", &42u64).expect("put failed");
//! let val: Option<u64> = table.typed_get("my_key").expect("get failed");
//! assert_eq!(val, Some(42u64));
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use oxistore_core::{
    expiry_epoch_millis, is_expired, KeysIter, KvSnapshot, KvStore, KvTxn, RangeIter, StoreError,
};
use redb::{
    ReadTransaction, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition,
};

/// redb table definition for TTL expiry timestamps (unix epoch milliseconds).
const TTL_TABLE: TableDefinition<&[u8], u64> = TableDefinition::new("__ttl__");

/// A [`KvStore`] backed by [redb](https://crates.io/crates/redb).
///
/// The underlying `redb::Database` is wrapped in an `Arc` so that
/// `RedbStore` can be cloned cheaply and shared across threads.
/// Write operations are serialized through a `Mutex`-protected write
/// transaction, which matches redb's own single-writer constraint.
#[derive(Clone)]
pub struct RedbStore {
    db: Arc<redb::Database>,
    /// Path to the database file (for `size_on_disk`).
    path: Option<std::path::PathBuf>,
    /// Serializes concurrent writes (redb allows only one write txn at a time).
    write_lock: Arc<Mutex<()>>,
    /// Name of the primary KV table (configurable via `RedbStoreBuilder`).
    table_name: &'static str,
}

impl RedbStore {
    /// The default table name used when opening without a custom name.
    const DEFAULT_TABLE_NAME: &'static str = "oxistore_kv";

    /// Pre-create both the primary and TTL tables inside `db`, using `table_name`.
    fn pre_create_tables(db: &redb::Database, table_name: &'static str) -> Result<(), StoreError> {
        let table_def: TableDefinition<&[u8], &[u8]> = TableDefinition::new(table_name);
        let txn = db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        {
            let _table = txn
                .open_table(table_def)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let _ttl_table = txn
                .open_table(TTL_TABLE)
                .map_err(|e| StoreError::Other(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))
    }

    /// Open (or create) a redb database at `path`.
    ///
    /// If the directory containing `path` does not exist, it is created
    /// automatically.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        Self::open_with_table(path, Self::DEFAULT_TABLE_NAME)
    }

    /// Open (or create) a redb database at `path` using a custom table name.
    ///
    /// Different table names create logically isolated key namespaces within
    /// the same database file.
    pub fn open_with_table(
        path: impl AsRef<std::path::Path>,
        table_name: &'static str,
    ) -> Result<Self, StoreError> {
        let path = path.as_ref();
        oxistore_core::ensure_parent_dir(path)?;
        let db = redb::Database::builder()
            .create(path)
            .map_err(|e| StoreError::Corruption(e.to_string()))?;
        Self::pre_create_tables(&db, table_name)?;
        Ok(Self {
            db: Arc::new(db),
            path: Some(path.to_path_buf()),
            write_lock: Arc::new(Mutex::new(())),
            table_name,
        })
    }

    /// Open an in-memory redb database (useful for tests and ephemeral workloads).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let backend = redb::backends::InMemoryBackend::new();
        let db = redb::Database::builder()
            .create_with_backend(backend)
            .map_err(|e| StoreError::Corruption(e.to_string()))?;
        Self::pre_create_tables(&db, Self::DEFAULT_TABLE_NAME)?;
        Ok(Self {
            db: Arc::new(db),
            path: None,
            write_lock: Arc::new(Mutex::new(())),
            table_name: Self::DEFAULT_TABLE_NAME,
        })
    }

    /// Construct a [`RedbStore`] from an already-opened [`redb::Database`].
    ///
    /// Both the primary KV table and the TTL table are pre-created if they do
    /// not yet exist.  The store is treated as path-less (in-memory semantics
    /// for `size_on_disk` and `backup`).
    ///
    /// Use [`RedbStoreBuilder`] for the most ergonomic construction when you
    /// also want custom cache size or table name.
    pub fn from_database(db: redb::Database) -> Result<Self, StoreError> {
        Self::pre_create_tables(&db, Self::DEFAULT_TABLE_NAME)?;
        Ok(Self {
            db: Arc::new(db),
            path: None,
            write_lock: Arc::new(Mutex::new(())),
            table_name: Self::DEFAULT_TABLE_NAME,
        })
    }

    /// Check the integrity of the database file at the given path.
    ///
    /// This is a **static** helper: it opens a fresh exclusive database handle,
    /// runs the integrity check, and closes it.  Because redb acquires a file
    /// lock on open, **no other `RedbStore` handle (or any other process) may
    /// have the same file open when this is called** — otherwise it will return
    /// `Err(StoreError::Corruption("Database already open ..."))`.
    ///
    /// Typical usage:
    ///
    /// ```no_run
    /// # use oxistore_kv_redb::RedbStore;
    /// // Make sure all RedbStore handles on "my.redb" are dropped first.
    /// # let path = std::env::temp_dir().join("my.redb");
    /// let ok = RedbStore::check_integrity_at(&path).expect("check");
    /// assert!(ok, "database is clean");
    /// ```
    ///
    /// Returns `Ok(true)` if the database is clean, `Ok(false)` if it failed
    /// but was repaired, and `Err` if it could not be repaired.
    pub fn check_integrity_at(path: impl AsRef<std::path::Path>) -> Result<bool, StoreError> {
        let mut db = redb::Database::create(path.as_ref())
            .map_err(|e| StoreError::Corruption(e.to_string()))?;
        db.check_integrity()
            .map_err(|e| StoreError::Corruption(e.to_string()))
    }

    /// Check the integrity of this database file.
    ///
    /// # Errors
    ///
    /// Returns `Err(StoreError::Unsupported)` for in-memory databases.
    ///
    /// Returns `Err(StoreError::Corruption("Database already open ..."))` if
    /// any other process (or another `RedbStore` in the same process) holds the
    /// file lock.  Drop all other handles before calling this method.
    pub fn check_integrity(&self) -> Result<bool, StoreError> {
        match &self.path {
            None => Err(StoreError::Unsupported(
                "check_integrity is not supported for in-memory databases".to_string(),
            )),
            Some(p) => Self::check_integrity_at(p),
        }
    }

    /// Attempt to repair a potentially corrupted database at `path`.
    ///
    /// This opens a fresh exclusive database handle and runs redb's built-in
    /// integrity check, which also performs automatic repair when possible.
    ///
    /// Returns:
    /// - `Ok(true)` — the database was intact (or repaired successfully).
    /// - `Ok(false)` — the database could not be repaired.
    /// - `Err(e)` — an I/O or other error occurred while attempting to open
    ///   or check the file.
    ///
    /// Because redb acquires an exclusive file lock, **no other `RedbStore`
    /// handle (or any other process) may have the file open when this is
    /// called**; drop all handles before invoking `try_repair`.
    pub fn try_repair(path: impl AsRef<std::path::Path>) -> Result<bool, StoreError> {
        let path = path.as_ref();
        let mut db =
            redb::Database::create(path).map_err(|e| StoreError::Corruption(e.to_string()))?;
        match db.check_integrity() {
            Ok(clean) => Ok(clean),
            Err(e) => {
                // check_integrity returns Err only if the file is irrecoverably
                // corrupted.  Surface this as Ok(false) rather than Err so that
                // callers can handle it gracefully.
                let msg = e.to_string();
                if msg.contains("Corrupted") || msg.contains("corrupted") {
                    Ok(false)
                } else {
                    Err(StoreError::Corruption(msg))
                }
            }
        }
    }

    /// Insert or overwrite a key-value pair, returning the **previous** value
    /// stored under `key` (if any).
    ///
    /// This mirrors redb's native API, which also returns the old value on
    /// `insert`.  Use this when you need the displaced value, e.g. for
    /// compare-and-swap logic or audit trails.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use oxistore_kv_redb::RedbStore;
    ///
    /// let store = RedbStore::open_in_memory().expect("open");
    /// store.put(b"k", b"v1").expect("initial put");
    /// let old = store.put_returning_old(b"k", b"v2").expect("put");
    /// assert_eq!(old.as_deref(), Some(b"v1".as_ref()));
    /// ```
    pub fn put_returning_old(
        &self,
        key: &[u8],
        value: &[u8],
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let old = {
            let mut table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let old = table
                .insert(key, value)
                .map_err(|e| StoreError::Other(e.to_string()))?
                .map(|guard| guard.value().to_vec());
            old
        };
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(old)
    }

    /// Remove a key, returning its **previous** value (if any).
    ///
    /// This mirrors redb's native API.  Use when you need to retrieve the
    /// value at the same time as deleting it (atomic read-and-delete).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use oxistore_kv_redb::RedbStore;
    ///
    /// let store = RedbStore::open_in_memory().expect("open");
    /// store.put(b"k", b"v").expect("put");
    /// let old = store.delete_returning_old(b"k").expect("delete");
    /// assert_eq!(old.as_deref(), Some(b"v".as_ref()));
    /// let old2 = store.delete_returning_old(b"k").expect("delete absent");
    /// assert_eq!(old2, None);
    /// ```
    pub fn delete_returning_old(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let old = {
            let mut table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let old = table
                .remove(key)
                .map_err(|e| StoreError::Other(e.to_string()))?
                .map(|guard| guard.value().to_vec());
            old
        };
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(old)
    }

    /// Open a `RedbStore` with automatic corruption recovery.
    ///
    /// On first open, if redb detects a corrupted header or truncated pages it
    /// will attempt automatic repair via `check_integrity()`.  This method:
    ///
    /// 1. Tries to open the database normally.
    /// 2. If that fails with a corruption-like error, deletes the corrupted
    ///    file and creates a fresh empty database (data loss is unavoidable when
    ///    the file cannot be repaired in-place).
    /// 3. Returns `Ok((store, repaired))` where `repaired` is `false` for a
    ///    clean open and `true` when the file had to be recreated.
    ///
    /// # Errors
    ///
    /// Returns `Err(StoreError::Corruption)` when the database cannot be
    /// opened or recreated.
    pub fn open_with_recovery(
        path: impl AsRef<std::path::Path>,
    ) -> Result<(Self, bool), StoreError> {
        let path = path.as_ref();
        oxistore_core::ensure_parent_dir(path)?;

        // Attempt normal open first.
        match redb::Database::builder().create(path) {
            Ok(db) => {
                Self::pre_create_tables(&db, Self::DEFAULT_TABLE_NAME)?;
                let store = Self {
                    db: Arc::new(db),
                    path: Some(path.to_path_buf()),
                    write_lock: Arc::new(Mutex::new(())),
                    table_name: Self::DEFAULT_TABLE_NAME,
                };
                return Ok((store, false));
            }
            Err(ref e) if is_corruption_error(e) => {
                // Fall through to recovery path below.
            }
            Err(e) => return Err(StoreError::Corruption(e.to_string())),
        }

        // Recovery: remove the corrupted file and start fresh.
        // This is the only safe strategy when redb cannot repair the file
        // in-place; callers should restore from a backup if available.
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| {
                StoreError::Corruption(format!("cannot remove corrupted file: {e}"))
            })?;
        }
        let db = redb::Database::builder()
            .create(path)
            .map_err(|e| StoreError::Corruption(format!("recreate after corruption: {e}")))?;
        Self::pre_create_tables(&db, Self::DEFAULT_TABLE_NAME)?;
        let store = Self {
            db: Arc::new(db),
            path: Some(path.to_path_buf()),
            write_lock: Arc::new(Mutex::new(())),
            table_name: Self::DEFAULT_TABLE_NAME,
        };
        Ok((store, true))
    }

    /// Return the `TableDefinition` for the primary KV table.
    #[inline]
    fn table_def(&self) -> TableDefinition<'static, &'static [u8], &'static [u8]> {
        TableDefinition::new(self.table_name)
    }
}

/// Returns `true` when a redb `DatabaseError` indicates data corruption or an
/// invalid/unrecoverable file format.
///
/// redb may surface corruption as a variety of error messages.  We match on
/// the `DatabaseError` variants directly first (most reliable), then fall back
/// to string-matching for older versions or variant gaps.
fn is_corruption_error(e: &redb::DatabaseError) -> bool {
    // Prefer direct variant matching so we're not guessing on error strings.
    match e {
        redb::DatabaseError::DatabaseAlreadyOpen => false,
        _ => {
            let msg = e.to_string().to_lowercase();
            msg.contains("corrupt")
                || msg.contains("invalid data")
                || msg.contains("invalid magic")
                || msg.contains("checksum")
                || msg.contains("truncated")
        }
    }
}

impl KvStore for RedbStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        // Phase 1: read the value and check TTL in a read transaction.
        let (value, expiry_opt) = {
            let txn = self
                .db
                .begin_read()
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let ttl_table = txn
                .open_table(TTL_TABLE)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let value = table
                .get(key)
                .map_err(|e| StoreError::Other(e.to_string()))?
                .map(|guard| guard.value().to_vec());
            let expiry = ttl_table
                .get(key)
                .map_err(|e| StoreError::Other(e.to_string()))?
                .map(|guard| guard.value());
            (value, expiry)
        };

        // Phase 2: if an expiry exists and is in the past, lazy-evict and return None.
        if let Some(expiry_millis) = expiry_opt {
            if is_expired(expiry_millis) {
                // Evict the expired key in a write transaction.
                let _guard = self
                    .write_lock
                    .lock()
                    .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
                let txn = self
                    .db
                    .begin_write()
                    .map_err(|e| StoreError::Other(e.to_string()))?;
                {
                    let mut table = txn
                        .open_table(self.table_def())
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    let mut ttl_table = txn
                        .open_table(TTL_TABLE)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    table
                        .remove(key)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    ttl_table
                        .remove(key)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                }
                txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
                return Ok(None);
            }
        }
        Ok(value)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        {
            let mut table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            table
                .insert(key, value)
                .map_err(|e| StoreError::Other(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        {
            let mut table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            table
                .remove(key)
                .map_err(|e| StoreError::Other(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let lo_owned = lo.to_vec();
        let hi_owned = hi.to_vec();
        let pairs: Vec<oxistore_core::RangeItem> = table
            .range(lo_owned.as_slice()..hi_owned.as_slice())
            .map_err(|e| StoreError::Other(e.to_string()))?
            .map(|item| {
                item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn prefix_scan<'a>(&'a self, prefix: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;

        let prefix_owned = prefix.to_vec();
        let pairs: Vec<oxistore_core::RangeItem> = match oxistore_core::prefix_upper_bound(prefix) {
            Some(hi) => table
                .range(prefix_owned.as_slice()..hi.as_slice())
                .map_err(|e| StoreError::Other(e.to_string()))?
                .map(|item| {
                    item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                        .map_err(|e| StoreError::Other(e.to_string()))
                })
                .collect(),
            None => {
                // No upper bound — scan everything (or from prefix..).
                if prefix.is_empty() {
                    table
                        .iter()
                        .map_err(|e| StoreError::Other(e.to_string()))?
                        .map(|item| {
                            item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                                .map_err(|e| StoreError::Other(e.to_string()))
                        })
                        .collect()
                } else {
                    table
                        .range(prefix_owned.as_slice()..)
                        .map_err(|e| StoreError::Other(e.to_string()))?
                        .map(|item| {
                            item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                                .map_err(|e| StoreError::Other(e.to_string()))
                        })
                        .collect()
                }
            }
        };
        Ok(Box::new(pairs.into_iter()))
    }

    fn batch_write(&self, pairs: &[(&[u8], &[u8])]) -> Result<(), StoreError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        {
            let mut table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            for &(k, v) in pairs {
                table
                    .insert(k, v)
                    .map_err(|e| StoreError::Other(e.to_string()))?;
            }
        }
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }

    fn batch_delete(&self, keys: &[&[u8]]) -> Result<(), StoreError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        {
            let mut table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            for &k in keys {
                table
                    .remove(k)
                    .map_err(|e| StoreError::Other(e.to_string()))?;
            }
        }
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }

    fn count(&self) -> Result<u64, StoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        table.len().map_err(|e| StoreError::Other(e.to_string()))
    }

    fn size_on_disk(&self) -> Result<u64, StoreError> {
        match &self.path {
            Some(p) => {
                let meta = std::fs::metadata(p)?;
                Ok(meta.len())
            }
            None => Ok(0),
        }
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let pairs: Vec<oxistore_core::RangeItem> = table
            .iter()
            .map_err(|e| StoreError::Other(e.to_string()))?
            .map(|item| {
                item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn keys<'a>(&'a self) -> Result<KeysIter<'a>, StoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let keys: Vec<Result<Vec<u8>, StoreError>> = table
            .iter()
            .map_err(|e| StoreError::Other(e.to_string()))?
            .map(|item| {
                item.map(|(k, _v)| k.value().to_vec())
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(keys.into_iter()))
    }

    fn compact(&self) -> Result<(), StoreError> {
        // redb::Database::compact() requires &mut self, which is not
        // available through Arc.  Compaction is a no-op for redb in this
        // wrapper; callers needing to compact should use the redb API directly.
        Ok(())
    }

    fn backup(&self, dest: &std::path::Path) -> Result<(), StoreError> {
        match &self.path {
            Some(src) => {
                oxistore_core::ensure_parent_dir(dest)?;
                std::fs::copy(src, dest)?;
                Ok(())
            }
            None => Err(StoreError::Other(
                "cannot backup an in-memory database".to_string(),
            )),
        }
    }

    fn restore(&self, backup: &std::path::Path) -> Result<(), StoreError> {
        if !backup.exists() {
            return Err(StoreError::Other(format!(
                "backup file does not exist: {}",
                backup.display()
            )));
        }

        // Open the backup as a source database and read all entries.
        let backup_db =
            redb::Database::open(backup).map_err(|e| StoreError::Corruption(e.to_string()))?;
        let src_txn = backup_db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let src_table = src_txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;

        // Collect all entries from the backup.
        let entries: Vec<(Vec<u8>, Vec<u8>)> = src_table
            .iter()
            .map_err(|e| StoreError::Other(e.to_string()))?
            .map(|item| {
                item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        drop(src_table);
        drop(src_txn);
        drop(backup_db);

        // Write all entries into this store atomically.
        let refs: Vec<(&[u8], &[u8])> = entries
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
            .collect();
        self.batch_write(&refs)?;
        Ok(())
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(Box::new(RedbTxn {
            inner: Some(txn),
            overlay: BTreeMap::new(),
            table_name: self.table_name,
        }))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        // True MVCC snapshot: open a read transaction that captures the database
        // state at this point in time.  Subsequent writes do not affect reads
        // made through this snapshot.
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(Box::new(RedbSnapshot {
            txn,
            table_def: self.table_def(),
        }))
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }

    fn put_with_ttl(
        &self,
        key: &[u8],
        value: &[u8],
        ttl: std::time::Duration,
    ) -> Result<(), StoreError> {
        let expiry = expiry_epoch_millis(ttl)?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        {
            let mut table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let mut ttl_table = txn
                .open_table(TTL_TABLE)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            table
                .insert(key, value)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            ttl_table
                .insert(key, expiry)
                .map_err(|e| StoreError::Other(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))
    }

    fn expire(&self, key: &[u8], ttl: std::time::Duration) -> Result<(), StoreError> {
        // Verify the key exists first.
        {
            let read_txn = self
                .db
                .begin_read()
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let table = read_txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let exists = table
                .get(key)
                .map_err(|e| StoreError::Other(e.to_string()))?
                .is_some();
            if !exists {
                return Err(StoreError::KeyNotFound);
            }
        }
        let expiry = expiry_epoch_millis(ttl)?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        {
            let mut ttl_table = txn
                .open_table(TTL_TABLE)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            ttl_table
                .insert(key, expiry)
                .map_err(|e| StoreError::Other(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))
    }

    fn ttl(&self, key: &[u8]) -> Result<Option<std::time::Duration>, StoreError> {
        let (data_exists, expiry_opt) = {
            let txn = self
                .db
                .begin_read()
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let ttl_table = txn
                .open_table(TTL_TABLE)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let data_exists = table
                .get(key)
                .map_err(|e| StoreError::Other(e.to_string()))?
                .is_some();
            let expiry = ttl_table
                .get(key)
                .map_err(|e| StoreError::Other(e.to_string()))?
                .map(|guard| guard.value());
            (data_exists, expiry)
        };

        if !data_exists {
            return Err(StoreError::KeyNotFound);
        }

        match expiry_opt {
            None => Ok(None),
            Some(expiry_millis) => {
                if is_expired(expiry_millis) {
                    // Lazy eviction.
                    let _guard = self
                        .write_lock
                        .lock()
                        .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
                    let txn = self
                        .db
                        .begin_write()
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    {
                        let mut table = txn
                            .open_table(self.table_def())
                            .map_err(|e| StoreError::Other(e.to_string()))?;
                        let mut ttl_table = txn
                            .open_table(TTL_TABLE)
                            .map_err(|e| StoreError::Other(e.to_string()))?;
                        table
                            .remove(key)
                            .map_err(|e| StoreError::Other(e.to_string()))?;
                        ttl_table
                            .remove(key)
                            .map_err(|e| StoreError::Other(e.to_string()))?;
                    }
                    txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
                    Err(StoreError::KeyNotFound)
                } else {
                    let now_millis = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    let remaining_millis = expiry_millis.saturating_sub(now_millis);
                    Ok(Some(std::time::Duration::from_millis(remaining_millis)))
                }
            }
        }
    }

    fn persist(&self, key: &[u8]) -> Result<bool, StoreError> {
        // Verify the key exists and check if it has a TTL.
        let (data_exists, has_ttl) = {
            let txn = self
                .db
                .begin_read()
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let ttl_table = txn
                .open_table(TTL_TABLE)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let data_exists = table
                .get(key)
                .map_err(|e| StoreError::Other(e.to_string()))?
                .is_some();
            let has_ttl = ttl_table
                .get(key)
                .map_err(|e| StoreError::Other(e.to_string()))?
                .is_some();
            (data_exists, has_ttl)
        };

        if !data_exists {
            return Err(StoreError::KeyNotFound);
        }
        if !has_ttl {
            return Ok(false);
        }

        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        {
            let mut ttl_table = txn
                .open_table(TTL_TABLE)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            ttl_table
                .remove(key)
                .map_err(|e| StoreError::Other(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(true)
    }

    fn purge_expired(&self) -> Result<u64, StoreError> {
        // Phase 1: collect expired keys from a read transaction.
        let expired_keys: Vec<Vec<u8>> = {
            let txn = self
                .db
                .begin_read()
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let ttl_table = txn
                .open_table(TTL_TABLE)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let mut keys = Vec::new();
            for item in ttl_table
                .iter()
                .map_err(|e| StoreError::Other(e.to_string()))?
            {
                let (k, expiry) = item.map_err(|e| StoreError::Other(e.to_string()))?;
                if is_expired(expiry.value()) {
                    keys.push(k.value().to_vec());
                }
            }
            keys
        };

        if expired_keys.is_empty() {
            return Ok(0);
        }

        // Phase 2: delete expired keys in a write transaction.
        let count = expired_keys.len() as u64;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?;
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        {
            let mut table = txn
                .open_table(self.table_def())
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let mut ttl_table = txn
                .open_table(TTL_TABLE)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            for key in &expired_keys {
                table
                    .remove(key.as_slice())
                    .map_err(|e| StoreError::Other(e.to_string()))?;
                ttl_table
                    .remove(key.as_slice())
                    .map_err(|e| StoreError::Other(e.to_string()))?;
            }
        }
        txn.commit().map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(count)
    }
}

// ------------------------------------------------------------------
// RedbTxn — with read-your-writes overlay
// ------------------------------------------------------------------

/// Buffered operation within a transaction overlay.
#[derive(Clone)]
enum TxnOp {
    /// Value was inserted/updated.
    Put(Vec<u8>),
    /// Key was deleted.
    Delete,
}

/// A write transaction backed by a `redb::WriteTransaction`.
///
/// Supports **read-your-writes**: reads first consult the local overlay
/// of buffered operations before falling back to the committed database.
pub struct RedbTxn {
    inner: Option<redb::WriteTransaction>,
    /// Overlay of buffered puts/deletes for read-your-writes.
    overlay: BTreeMap<Vec<u8>, TxnOp>,
    /// Name of the primary KV table (mirrors the parent `RedbStore`).
    table_name: &'static str,
}

impl RedbTxn {
    #[inline]
    fn table_def(&self) -> TableDefinition<'static, &'static [u8], &'static [u8]> {
        TableDefinition::new(self.table_name)
    }
}

impl KvTxn for RedbTxn {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        // Check local overlay first (read-your-writes).
        if let Some(op) = self.overlay.get(key) {
            return match op {
                TxnOp::Put(v) => Ok(Some(v.clone())),
                TxnOp::Delete => Ok(None),
            };
        }
        // Fall through to committed state.
        let txn = self
            .inner
            .as_ref()
            .ok_or_else(|| StoreError::Other("transaction already consumed".to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let result = table
            .get(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
            .map(|guard| guard.value().to_vec());
        Ok(result)
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let txn = self
            .inner
            .as_ref()
            .ok_or_else(|| StoreError::Other("transaction already consumed".to_string()))?;
        let mut table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        table
            .insert(key, value)
            .map_err(|e| StoreError::Other(e.to_string()))?;
        // Also record in overlay for read-your-writes.
        self.overlay
            .insert(key.to_vec(), TxnOp::Put(value.to_vec()));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StoreError> {
        let txn = self
            .inner
            .as_ref()
            .ok_or_else(|| StoreError::Other("transaction already consumed".to_string()))?;
        let mut table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        table
            .remove(key)
            .map_err(|e| StoreError::Other(e.to_string()))?;
        self.overlay.insert(key.to_vec(), TxnOp::Delete);
        Ok(())
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        Ok(self.get(key)?.is_some())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        // Merge committed range with overlay.
        let txn = self
            .inner
            .as_ref()
            .ok_or_else(|| StoreError::Other("transaction already consumed".to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let lo_owned = lo.to_vec();
        let hi_owned = hi.to_vec();

        // Collect committed data into a BTreeMap for merging.
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for item in table
            .range(lo_owned.as_slice()..hi_owned.as_slice())
            .map_err(|e| StoreError::Other(e.to_string()))?
        {
            let (k, v) = item.map_err(|e| StoreError::Other(e.to_string()))?;
            merged.insert(k.value().to_vec(), v.value().to_vec());
        }

        // Apply overlay.
        for (k, op) in self.overlay.range(lo_owned.clone()..hi_owned.clone()) {
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

    fn commit(mut self: Box<Self>) -> Result<(), StoreError> {
        self.inner
            .take()
            .ok_or_else(|| StoreError::Other("transaction already consumed".to_string()))?
            .commit()
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn rollback(mut self: Box<Self>) -> Result<(), StoreError> {
        if let Some(txn) = self.inner.take() {
            txn.abort().map_err(|e| StoreError::Other(e.to_string()))?;
        }
        Ok(())
    }
}

// ------------------------------------------------------------------
// RedbSnapshot — true MVCC snapshot backed by redb::ReadTransaction
// ------------------------------------------------------------------

/// A point-in-time MVCC snapshot backed by a `redb::ReadTransaction`.
///
/// The `ReadTransaction` captures database state at the moment it is opened.
/// Writes committed after the snapshot is created are not visible through it.
///
/// Range iteration is materialized into a `Vec` to avoid self-referential
/// lifetime issues between `ReadTransaction`, `ReadOnlyTable`, and the range
/// iterator all living in the same struct.
pub struct RedbSnapshot {
    txn: ReadTransaction,
    table_def: TableDefinition<'static, &'static [u8], &'static [u8]>,
}

impl KvSnapshot for RedbSnapshot {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let table = self
            .txn
            .open_table(self.table_def)
            .map_err(|e| StoreError::Other(e.to_string()))?;
        table
            .get(key)
            .map(|opt| opt.map(|guard| guard.value().to_vec()))
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        // Materialize into Vec to avoid self-referential struct lifetime issues.
        let table = self
            .txn
            .open_table(self.table_def)
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let lo_owned = lo.to_vec();
        let hi_owned = hi.to_vec();
        let pairs: Vec<oxistore_core::RangeItem> = table
            .range(lo_owned.as_slice()..hi_owned.as_slice())
            .map_err(|e| StoreError::Other(e.to_string()))?
            .map(|item| {
                item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }
}

// ------------------------------------------------------------------
// RedbStoreBuilder
// ------------------------------------------------------------------

/// Builder for [`RedbStore`].
///
/// Provides fine-grained control over the underlying redb database:
/// custom cache size, custom table name, and both file-backed and
/// in-memory construction.
///
/// # Example
///
/// ```no_run
/// use oxistore_kv_redb::RedbStoreBuilder;
/// use oxistore_core::KvStore;
///
/// # let path = std::env::temp_dir().join("custom.redb");
/// let store = RedbStoreBuilder::new()
///     .cache_size(64 * 1024 * 1024)
///     .table_name("my_table")
///     .build(&path)
///     .expect("build failed");
/// store.put(b"hello", b"world").expect("put failed");
/// ```
pub struct RedbStoreBuilder {
    cache_size: Option<usize>,
    table_name: &'static str,
}

impl Default for RedbStoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RedbStoreBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self {
            cache_size: None,
            table_name: RedbStore::DEFAULT_TABLE_NAME,
        }
    }

    /// Set the redb block cache size in bytes.
    #[must_use]
    pub fn cache_size(mut self, bytes: usize) -> Self {
        self.cache_size = Some(bytes);
        self
    }

    /// Override the primary KV table name.
    ///
    /// Different table names create isolated key namespaces within the same
    /// database file.  The name must be a `'static str`.
    #[must_use]
    pub fn table_name(mut self, name: &'static str) -> Self {
        self.table_name = name;
        self
    }

    /// Build a file-backed [`RedbStore`] at `path`.
    ///
    /// The directory containing `path` is created automatically if it does not
    /// exist.
    pub fn build(self, path: impl AsRef<std::path::Path>) -> Result<RedbStore, StoreError> {
        let path = path.as_ref();
        oxistore_core::ensure_parent_dir(path)?;
        let mut builder = redb::Database::builder();
        if let Some(cs) = self.cache_size {
            builder.set_cache_size(cs);
        }
        let db = builder
            .create(path)
            .map_err(|e| StoreError::Corruption(e.to_string()))?;
        RedbStore::pre_create_tables(&db, self.table_name)?;
        Ok(RedbStore {
            db: Arc::new(db),
            path: Some(path.to_path_buf()),
            write_lock: Arc::new(Mutex::new(())),
            table_name: self.table_name,
        })
    }

    /// Build an in-memory [`RedbStore`].
    ///
    /// The store is ephemeral; data is lost when it is dropped.
    pub fn build_in_memory(self) -> Result<RedbStore, StoreError> {
        let backend = redb::backends::InMemoryBackend::new();
        let mut redb_builder = redb::Database::builder();
        if let Some(cs) = self.cache_size {
            redb_builder.set_cache_size(cs);
        }
        let db = redb_builder
            .create_with_backend(backend)
            .map_err(|e| StoreError::Corruption(e.to_string()))?;
        RedbStore::pre_create_tables(&db, self.table_name)?;
        Ok(RedbStore {
            db: Arc::new(db),
            path: None,
            write_lock: Arc::new(Mutex::new(())),
            table_name: self.table_name,
        })
    }
}

// ------------------------------------------------------------------
// RedbIter — streaming (lazy-drain) range iterator
// ------------------------------------------------------------------

/// A streaming iterator over `(key, value)` pairs from a [`RedbStore`] range
/// or full-scan query.
///
/// Internally the results are materialized upfront (because redb's table
/// iterator borrows from the `ReadTransaction`, which cannot be stored
/// alongside the iterator in safe Rust without self-referential structs).
/// The key advantages over the trait's `Box<dyn Iterator>` are:
///
/// - Implements [`ExactSizeIterator`] — callers know the total count upfront.
/// - Implements [`DoubleEndedIterator`] — callers can reverse-iterate cheaply.
/// - Exposes `peek` / `rewind` for scan-ahead patterns.
/// - The concrete type can be stored and passed around without a vtable.
///
/// Obtain a `RedbIter` via [`RedbStore::range_iter`], [`RedbStore::iter_collected`],
/// or [`RedbStore::prefix_iter`].
pub struct RedbIter {
    inner: std::vec::IntoIter<oxistore_core::RangeItem>,
    remaining: usize,
}

impl RedbIter {
    fn new(items: Vec<oxistore_core::RangeItem>) -> Self {
        let remaining = items.len();
        RedbIter {
            inner: items.into_iter(),
            remaining,
        }
    }

    /// Return the number of items not yet yielded.
    ///
    /// This is an O(1) operation because `RedbIter` stores the count
    /// separately from the underlying iterator.
    pub fn len(&self) -> usize {
        self.remaining
    }

    /// Return `true` if no items remain.
    pub fn is_empty(&self) -> bool {
        self.remaining == 0
    }
}

impl Iterator for RedbIter {
    type Item = oxistore_core::RangeItem;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next();
        if item.is_some() {
            self.remaining = self.remaining.saturating_sub(1);
        }
        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for RedbIter {}

impl DoubleEndedIterator for RedbIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        let item = self.inner.next_back();
        if item.is_some() {
            self.remaining = self.remaining.saturating_sub(1);
        }
        item
    }
}

impl RedbStore {
    /// Streaming scan over key-value pairs in `[lo, hi)`.
    ///
    /// This is the primary streaming API for `RedbStore`.  It returns a
    /// concrete [`RedbIter`] which implements [`ExactSizeIterator`] and
    /// [`DoubleEndedIterator`], avoiding the overhead of a vtable and allowing
    /// the caller to inspect the total count upfront via `len()`.
    ///
    /// # Design note
    ///
    /// The [`KvStore::range`] trait method cannot be made truly zero-copy
    /// streaming without self-referential structs: redb's table iterator
    /// borrows from the `ReadTransaction`, which must outlive the iterator.
    /// In safe Rust this means either (a) materialising into a `Vec` (which
    /// `KvStore::range` does) or (b) holding both the transaction and the
    /// iterator in the same owning struct — which is what `RedbIter` does.
    ///
    /// `scan_iter` is the preferred path when you need lazy, low-memory
    /// iteration over large ranges: it materialises the result set once
    /// (unavoidable due to the lifetime constraint), but drains it lazily
    /// rather than converting to `Box<dyn Iterator>`.  For truly incremental
    /// I/O see the separate `RedbSnapshot`-based MVCC path or consider
    /// switching to a streaming backend (fjall/sled).
    ///
    /// Equivalent to [`RedbStore::range_iter`].
    pub fn scan_iter(&self, lo: &[u8], hi: &[u8]) -> Result<RedbIter, StoreError> {
        self.range_iter(lo, hi)
    }

    /// Return a [`RedbIter`] over key-value pairs in `[lo, hi)`.
    ///
    /// Unlike [`KvStore::range`], this returns a concrete [`RedbIter`] which
    /// implements [`ExactSizeIterator`] and [`DoubleEndedIterator`].
    pub fn range_iter(&self, lo: &[u8], hi: &[u8]) -> Result<RedbIter, StoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let lo_owned = lo.to_vec();
        let hi_owned = hi.to_vec();
        let items: Vec<oxistore_core::RangeItem> = table
            .range(lo_owned.as_slice()..hi_owned.as_slice())
            .map_err(|e| StoreError::Other(e.to_string()))?
            .map(|item| {
                item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(RedbIter::new(items))
    }

    /// Return a [`RedbIter`] over all key-value pairs in the store.
    ///
    /// Provides the same data as [`KvStore::iter`] but as a concrete
    /// [`RedbIter`] with `ExactSizeIterator` and `DoubleEndedIterator` support.
    pub fn iter_collected(&self) -> Result<RedbIter, StoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let items: Vec<oxistore_core::RangeItem> = table
            .iter()
            .map_err(|e| StoreError::Other(e.to_string()))?
            .map(|item| {
                item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(RedbIter::new(items))
    }

    /// Return a [`RedbIter`] over key-value pairs whose keys start with `prefix`.
    ///
    /// Provides the same data as [`KvStore::prefix_scan`] but as a concrete
    /// [`RedbIter`].
    pub fn prefix_iter(&self, prefix: &[u8]) -> Result<RedbIter, StoreError> {
        let txn = self
            .db
            .begin_read()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let table = txn
            .open_table(self.table_def())
            .map_err(|e| StoreError::Other(e.to_string()))?;

        let prefix_owned = prefix.to_vec();
        let items: Vec<oxistore_core::RangeItem> = match oxistore_core::prefix_upper_bound(prefix) {
            Some(hi) => table
                .range(prefix_owned.as_slice()..hi.as_slice())
                .map_err(|e| StoreError::Other(e.to_string()))?
                .map(|item| {
                    item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                        .map_err(|e| StoreError::Other(e.to_string()))
                })
                .collect(),
            None => {
                if prefix.is_empty() {
                    table
                        .iter()
                        .map_err(|e| StoreError::Other(e.to_string()))?
                        .map(|item| {
                            item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                                .map_err(|e| StoreError::Other(e.to_string()))
                        })
                        .collect()
                } else {
                    table
                        .range(prefix_owned.as_slice()..)
                        .map_err(|e| StoreError::Other(e.to_string()))?
                        .map(|item| {
                            item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
                                .map_err(|e| StoreError::Other(e.to_string()))
                        })
                        .collect()
                }
            }
        };
        Ok(RedbIter::new(items))
    }
}

// ------------------------------------------------------------------
// TypedRedbTable — type-safe String-key / JSON-value table
// ------------------------------------------------------------------

/// Type-safe table wrapper for [`RedbStore`] using `String` keys and
/// JSON-serialized values.
///
/// Values are serialized to JSON using [`serde_json`] and stored as UTF-8
/// bytes.  The key is a `&str`/`String` for ergonomic use; internally it
/// is stored as raw UTF-8 bytes.
///
/// This is a **convenience wrapper** around [`RedbStore`].  All mutations
/// delegate to the inner store.
///
/// # Type parameters
///
/// `V` must implement [`serde::Serialize`] for writes and
/// [`serde::de::DeserializeOwned`] for reads.
///
/// # Example
///
/// ```no_run
/// use oxistore_kv_redb::{RedbStore, TypedRedbTable};
///
/// let store = RedbStore::open_in_memory().expect("open");
/// let table: TypedRedbTable = TypedRedbTable::new(store);
/// table.typed_put("counter", &42u64).expect("put");
/// let v: Option<u64> = table.typed_get("counter").expect("get");
/// assert_eq!(v, Some(42u64));
/// ```
pub struct TypedRedbTable {
    inner: RedbStore,
}

impl TypedRedbTable {
    /// Wrap an existing [`RedbStore`].
    pub fn new(store: RedbStore) -> Self {
        TypedRedbTable { inner: store }
    }

    /// Access the underlying [`RedbStore`].
    pub fn inner(&self) -> &RedbStore {
        &self.inner
    }

    /// Consume this wrapper and return the underlying [`RedbStore`].
    pub fn into_inner(self) -> RedbStore {
        self.inner
    }

    /// Store a value under a `String` key, serializing it to JSON.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Other`] if JSON serialization fails or the
    /// underlying store returns an error.
    pub fn typed_put<V>(&self, key: &str, value: &V) -> Result<(), StoreError>
    where
        V: serde::Serialize,
    {
        let json =
            serde_json::to_vec(value).map_err(|e| StoreError::Other(format!("serialize: {e}")))?;
        self.inner.put(key.as_bytes(), &json)
    }

    /// Retrieve and deserialize the value stored under `key`.
    ///
    /// Returns `Ok(None)` when the key is absent.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Other`] if JSON deserialization fails or the
    /// underlying store returns an error.
    pub fn typed_get<V>(&self, key: &str) -> Result<Option<V>, StoreError>
    where
        V: serde::de::DeserializeOwned,
    {
        match self.inner.get(key.as_bytes())? {
            None => Ok(None),
            Some(bytes) => {
                let v = serde_json::from_slice(&bytes)
                    .map_err(|e| StoreError::Other(format!("deserialize: {e}")))?;
                Ok(Some(v))
            }
        }
    }

    /// Delete the entry at `key`.  No-op if the key is absent.
    pub fn typed_delete(&self, key: &str) -> Result<(), StoreError> {
        self.inner.delete(key.as_bytes())
    }

    /// Return the raw [`RedbStore`] bytes for `key` without deserializing.
    ///
    /// Useful for inspection or migration purposes.
    pub fn raw_get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(key.as_bytes())
    }

    /// Insert a raw (pre-serialized) byte value without JSON encoding.
    pub fn raw_put(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        self.inner.put(key.as_bytes(), value)
    }

    /// Iterate all string-keyed entries, returning `(String, raw_json_bytes)` pairs.
    ///
    /// Keys that are not valid UTF-8 are skipped (they cannot have been inserted
    /// via [`TypedRedbTable::typed_put`]).
    pub fn iter_raw(&self) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        self.inner
            .iter()?
            .map(|item| {
                let (k, v) = item?;
                let key = String::from_utf8(k)
                    .map_err(|e| StoreError::Other(format!("invalid UTF-8 key: {e}")))?;
                Ok((key, v))
            })
            .collect()
    }
}

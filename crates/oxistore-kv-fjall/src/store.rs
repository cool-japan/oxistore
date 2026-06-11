use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex};

use fjall::{
    config::{BloomConstructionPolicy, CompressionPolicy, FilterPolicy, FilterPolicyEntry},
    CompressionType, Config, Database, Keyspace, KeyspaceCreateOptions, PersistMode, Readable,
};
use oxistore_core::{
    expiry_epoch_millis, is_expired, KeysIter, KvSnapshot, KvStore, KvTxn, RangeIter, StoreError,
};

use crate::FjallStoreError;

/// Type alias for the write-spec parameter of [`FjallStore::batch_write_across`].
///
/// Each element is `(partition_name, pairs)` where `pairs` is a list of
/// `(key, value)` byte slices to insert into that partition.
type PartitionWrites<'a> = [(&'a str, Vec<(&'a [u8], &'a [u8])>)];

// ------------------------------------------------------------------
// FjallStore
// ------------------------------------------------------------------

/// A [`KvStore`] backed by [fjall](https://crates.io/crates/fjall).
///
/// All data is stored in a single keyspace named `"default"`.  The
/// [`Database`] handle is wrapped in an [`Arc`] so that [`FjallStore`]
/// can be cloned cheaply and shared across threads.  fjall's `Keyspace`
/// is itself `Send + Sync`, so no additional locking is required for
/// read/write operations.
///
/// Write batches (used by [`KvTxn`]) are serialised through a `Mutex` to
/// ensure they are never committed concurrently, which would violate fjall's
/// single-journal-writer guarantee.
/// Encode an expiry timestamp as 8 little-endian bytes.
fn encode_expiry(millis: u64) -> [u8; 8] {
    millis.to_le_bytes()
}

/// Decode an 8-byte little-endian expiry timestamp.
fn decode_expiry(b: &[u8]) -> Option<u64> {
    b.try_into().ok().map(u64::from_le_bytes)
}

/// A [`KvStore`] backed by [fjall](https://crates.io/crates/fjall).
///
/// All data is stored in a single keyspace named `"default"`.  The
/// [`Database`] handle is wrapped in an [`Arc`] so that [`FjallStore`]
/// can be cloned cheaply and shared across threads.  fjall's `Keyspace`
/// is itself `Send + Sync`, so no additional locking is required for
/// read/write operations.
///
/// Write batches (used by [`KvTxn`]) are serialised through a `Mutex` to
/// ensure they are never committed concurrently, which would violate fjall's
/// single-journal-writer guarantee.
///
/// TTL expiry timestamps are stored in a separate `"__ttl__"` keyspace.
#[derive(Clone)]
pub struct FjallStore {
    db: Database,
    keyspace: Keyspace,
    /// Separate keyspace for TTL expiry timestamps.
    ttl_keyspace: Keyspace,
    /// Path to the database directory (for `size_on_disk`).
    path: std::path::PathBuf,
    /// Serialises batched write-transaction commits.
    txn_lock: Arc<Mutex<()>>,
}

impl FjallStore {
    /// Open (or create) a fjall database at `path`.
    ///
    /// If the directory does not exist it is created automatically.
    ///
    /// # Errors
    ///
    /// Returns [`FjallStoreError::Open`] if the database cannot be opened.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FjallStoreError> {
        let path = path.as_ref();

        // Create the directory tree if it does not exist.
        std::fs::create_dir_all(path).map_err(|e| FjallStoreError::Open(e.to_string()))?;

        let config = Config::new(path);
        let db = Database::open(config).map_err(|e| FjallStoreError::Open(e.to_string()))?;

        let keyspace = db
            .keyspace("default", KeyspaceCreateOptions::default)
            .map_err(|e| FjallStoreError::Open(e.to_string()))?;

        let ttl_keyspace = db
            .keyspace("__ttl__", KeyspaceCreateOptions::default)
            .map_err(|e| FjallStoreError::Open(e.to_string()))?;

        Ok(Self {
            db,
            keyspace,
            ttl_keyspace,
            path: path.to_path_buf(),
            txn_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Open an ephemeral fjall store in a unique temporary directory.
    ///
    /// The temporary directory is created under [`std::env::temp_dir()`] and
    /// is **not** automatically removed on drop — call `destroy` via the
    /// `oxistore` facade, or remove the directory manually.
    ///
    /// This is primarily intended for tests and ephemeral workloads.
    ///
    /// # Errors
    ///
    /// Returns [`FjallStoreError::Open`] if the temporary directory cannot be
    /// created or the database cannot be opened.
    pub fn open_in_memory() -> Result<Self, FjallStoreError> {
        // fjall does not support a true in-memory backend; use a unique temp dir.
        let dir = std::env::temp_dir().join(format!(
            "oxistore_fjall_mem_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        Self::open(&dir)
    }

    /// Construct a [`FjallStore`] from an existing [`Database`] handle by
    /// opening (or creating) the named keyspace as the primary data partition.
    ///
    /// This is useful when the caller already holds a `Database` reference and
    /// wants to wrap it in a [`FjallStore`] without taking ownership of the
    /// path.  The `"__ttl__"` keyspace is always opened alongside.
    ///
    /// `db_path` should be the directory path that was originally used when
    /// opening `db`; it is recorded for [`KvStore::size_on_disk`].
    ///
    /// # Errors
    ///
    /// Returns [`FjallStoreError::Open`] if the requested keyspace (or the
    /// internal TTL keyspace) cannot be opened.
    pub fn from_database(
        db: Database,
        keyspace_name: &str,
        db_path: impl AsRef<Path>,
    ) -> Result<Self, FjallStoreError> {
        let keyspace = db
            .keyspace(keyspace_name, KeyspaceCreateOptions::default)
            .map_err(|e| FjallStoreError::Open(e.to_string()))?;

        let ttl_keyspace = db
            .keyspace("__ttl__", KeyspaceCreateOptions::default)
            .map_err(|e| FjallStoreError::Open(e.to_string()))?;

        Ok(Self {
            db,
            keyspace,
            ttl_keyspace,
            path: db_path.as_ref().to_path_buf(),
            txn_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Persist the database journal to durable storage (full fsync).
    ///
    /// # Errors
    ///
    /// Returns [`FjallStoreError::Persist`] if the fsync fails.
    pub fn persist_sync(&self) -> Result<(), FjallStoreError> {
        self.db
            .persist(PersistMode::SyncAll)
            .map_err(|e| FjallStoreError::Persist(e.to_string()))
    }

    /// Obtain a cross-keyspace snapshot.
    ///
    /// The snapshot reflects a consistent view of the database at the moment
    /// this method is called.
    #[must_use]
    pub fn raw_snapshot(&self) -> fjall::Snapshot {
        self.db.snapshot()
    }

    // ------------------------------------------------------------------
    // Extended fjall-specific APIs
    // ------------------------------------------------------------------

    /// Open or create a named keyspace (column family) in this database.
    ///
    /// fjall keyspaces map naturally to column families: each keyspace is an
    /// independent LSM-tree backed partition with its own key space.  Keys
    /// written to one partition are invisible in any other partition.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use oxistore_kv_fjall::FjallStore;
    /// # use fjall::Readable;
    /// let store = FjallStore::open("/tmp/fjall-cf").unwrap();
    /// let family_a = store.open_partition("family_a").unwrap();
    /// let family_b = store.open_partition("family_b").unwrap();
    /// family_a.insert(b"key", b"from-a").unwrap();
    /// // family_b.get(b"key") returns None
    /// ```
    pub fn open_partition(&self, name: &str) -> Result<Keyspace, FjallStoreError> {
        self.db
            .keyspace(name, KeyspaceCreateOptions::default)
            .map_err(|e| FjallStoreError::Open(e.to_string()))
    }

    /// Back up the default keyspace to `path` using a length-prefixed binary format.
    ///
    /// The format written is:
    /// ```text
    /// For each key-value pair:
    ///   [key_len:   u32 LE] [key_bytes]
    ///   [value_len: u32 LE] [value_bytes]
    /// ```
    ///
    /// Restore with [`FjallStore::restore_from_backup`].
    pub fn backup(&self, path: &Path) -> Result<(), StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(path)?;
        for guard in self.keyspace.iter() {
            let (k, v) = guard
                .into_inner()
                .map_err(|e| StoreError::Other(e.to_string()))?;
            let key = k.as_ref();
            let val = v.as_ref();
            let key_len = key.len() as u32;
            let val_len = val.len() as u32;
            file.write_all(&key_len.to_le_bytes())?;
            file.write_all(key)?;
            file.write_all(&val_len.to_le_bytes())?;
            file.write_all(val)?;
        }
        file.flush()?;
        Ok(())
    }

    /// Restore key-value pairs from a backup file produced by [`FjallStore::backup`].
    ///
    /// Opens (or creates) a [`FjallStore`] at `dest_path` and inserts all
    /// records found in the backup file.  The destination store is returned.
    pub fn restore_from_backup(path: &Path, dest_path: &Path) -> Result<FjallStore, StoreError> {
        let dest_store =
            FjallStore::open(dest_path).map_err(|e| StoreError::Other(e.to_string()))?;
        let mut file = std::fs::File::open(path)?;
        loop {
            // Read key length.
            let mut len_buf = [0u8; 4];
            match file.read_exact(&mut len_buf) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(StoreError::Io(Arc::new(e))),
            }
            let key_len = u32::from_le_bytes(len_buf) as usize;
            let mut key = vec![0u8; key_len];
            file.read_exact(&mut key)?;

            // Read value length.
            file.read_exact(&mut len_buf)?;
            let val_len = u32::from_le_bytes(len_buf) as usize;
            let mut val = vec![0u8; val_len];
            file.read_exact(&mut val)?;

            dest_store.put(&key, &val)?;
        }
        Ok(dest_store)
    }

    /// Return the names of all keyspaces (column families) currently open in
    /// this database.
    ///
    /// The list always contains at least `"default"` and `"__ttl__"`, which
    /// are the keyspaces managed internally by [`FjallStore`].  Any additional
    /// partitions opened via [`FjallStore::open_partition`] are also included.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use oxistore_kv_fjall::FjallStore;
    /// let store = FjallStore::open("/tmp/fjall-ks").unwrap();
    /// let names = store.list_keyspaces().unwrap();
    /// assert!(names.contains(&"default".to_string()));
    /// ```
    pub fn list_keyspaces(&self) -> Result<Vec<String>, FjallStoreError> {
        let names = self
            .db
            .list_keyspace_names()
            .into_iter()
            .map(|k| k.to_string())
            .collect();
        Ok(names)
    }

    /// Write key-value pairs atomically across multiple named partitions in a
    /// single fjall `WriteBatch`.
    ///
    /// Each entry in `writes` is a `(partition_name, pairs)` tuple where
    /// `pairs` is a slice of `(key, value)` byte-slice pairs to insert into
    /// that partition.  All partitions are opened (or created) on demand using
    /// default options; if a partition with the given name already exists, its
    /// existing configuration is preserved.
    ///
    /// The entire multi-partition write is committed atomically: either all
    /// inserts land or none do.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Other`] if any partition cannot be opened or if
    /// the batch commit fails.
    pub fn batch_write_across(&self, writes: &PartitionWrites<'_>) -> Result<(), StoreError> {
        // Open all target partitions first so they outlive the batch.
        let partitions: Vec<Keyspace> = writes
            .iter()
            .map(|(name, _)| {
                self.db
                    .keyspace(name, KeyspaceCreateOptions::default)
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut batch = self.db.batch();
        for (partition, (_, pairs)) in partitions.iter().zip(writes.iter()) {
            for &(k, v) in pairs {
                batch.insert(partition, k, v);
            }
        }
        batch.commit().map_err(|e| StoreError::Other(e.to_string()))
    }

    /// Insert or overwrite a key-value pair, returning the **previous** value
    /// that was associated with the key (or `None` if the key was absent).
    ///
    /// Unlike [`KvStore::put`], this method performs a read-then-write cycle
    /// within a single thread.  It is **not** atomic with respect to concurrent
    /// writers — callers that need compare-and-swap semantics should use
    /// [`KvStore::compare_and_swap`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if either the read or the write fails.
    pub fn put_returning(&self, key: &[u8], value: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let prev = self.get(key)?;
        self.put(key, value)?;
        Ok(prev)
    }

    /// Remove a key, returning the **previous** value that was associated
    /// with it (or `None` if the key was already absent).
    ///
    /// Like `put_returning`, this performs a read-then-delete cycle and is
    /// **not** atomic with respect to concurrent writers.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if either the read or the delete fails.
    pub fn delete_returning(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let prev = self.get(key)?;
        if prev.is_some() {
            self.delete(key)?;
        }
        Ok(prev)
    }

    /// Apply a write-rate limit to subsequent writes by inserting a small sleep
    /// between every `writes_per_period` individual `put` calls.
    ///
    /// fjall's own LSM engine does **not** expose a built-in write-rate-limiter
    /// API in version 3.x.  This helper provides a *software-level* token-bucket
    /// approximation: callers call `rate_limited_put` via [`FjallStore::rate_limiter`] instead of
    /// the raw [`KvStore::put`] to honour the configured limit.
    ///
    /// Returns a [`RateLimitedWriter`] that wraps `self` and enforces the limit.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use oxistore_kv_fjall::FjallStore;
    /// # use std::time::Duration;
    /// let store = FjallStore::open_in_memory().unwrap();
    /// let writer = store.rate_limiter(500, Duration::from_secs(1));
    /// writer.put(b"k", b"v").unwrap();
    /// ```
    #[must_use]
    pub fn rate_limiter(
        &self,
        writes_per_period: u64,
        period: std::time::Duration,
    ) -> RateLimitedWriter<'_> {
        RateLimitedWriter {
            store: self,
            writes_per_period,
            period,
            counter: 0,
        }
    }
}

// ------------------------------------------------------------------
// RateLimitedWriter — software token-bucket write rate limiter
// ------------------------------------------------------------------

/// A thin wrapper around [`FjallStore`] that enforces a software-level
/// write rate limit.
///
/// After every `writes_per_period` calls to [`RateLimitedWriter::put`] the
/// writer sleeps for `period` milliseconds to approximate the target
/// throughput.  Because fjall does not expose a native write-rate-limiter in
/// its 3.x API, this is a best-effort approximation at the application layer.
///
/// Obtain an instance via [`FjallStore::rate_limiter`].
pub struct RateLimitedWriter<'a> {
    store: &'a FjallStore,
    /// Number of writes allowed per `period` before a sleep is inserted.
    writes_per_period: u64,
    /// Duration of the sleep inserted after every `writes_per_period` writes.
    period: std::time::Duration,
    /// Running count of writes issued since the last sleep (not persisted).
    counter: u64,
}

impl RateLimitedWriter<'_> {
    /// Write a key-value pair, sleeping if the rate limit has been reached.
    ///
    /// # Errors
    ///
    /// Propagates any [`StoreError`] returned by the underlying store.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.store.put(key, value)?;
        self.counter += 1;
        if self.writes_per_period > 0 && self.counter >= self.writes_per_period {
            std::thread::sleep(self.period);
            self.counter = 0;
        }
        Ok(())
    }

    /// Delete a key, counting towards the rate limit.
    ///
    /// # Errors
    ///
    /// Propagates any [`StoreError`] returned by the underlying store.
    pub fn delete(&mut self, key: &[u8]) -> Result<(), StoreError> {
        self.store.delete(key)?;
        self.counter += 1;
        if self.writes_per_period > 0 && self.counter >= self.writes_per_period {
            std::thread::sleep(self.period);
            self.counter = 0;
        }
        Ok(())
    }
}

/// Compute directory size recursively (helper for `size_on_disk`).
fn dir_size(path: &Path) -> Result<u64, std::io::Error> {
    let mut total = 0u64;
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_file() {
                total += entry.metadata()?.len();
            } else if ft.is_dir() {
                total += dir_size(&entry.path())?;
            }
        }
    }
    Ok(total)
}

impl KvStore for FjallStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        // Check TTL expiry before returning the value.
        if let Some(expiry_bytes) = self
            .ttl_keyspace
            .get(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
        {
            if let Some(expiry_millis) = decode_expiry(&expiry_bytes) {
                if is_expired(expiry_millis) {
                    // Lazy eviction via batch.
                    let mut batch = self.db.batch();
                    batch.remove(&self.keyspace, key);
                    batch.remove(&self.ttl_keyspace, key);
                    batch
                        .commit()
                        .map_err(|e| StoreError::Other(e.to_string()))?;
                    return Ok(None);
                }
            }
        }
        self.keyspace
            .get(key)
            .map(|opt| opt.map(|v| v.to_vec()))
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.keyspace
            .insert(key, value)
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.keyspace
            .remove(key)
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let lo_owned = lo.to_vec();
        let hi_owned = hi.to_vec();
        let pairs: Vec<oxistore_core::RangeItem> = self
            .keyspace
            .range(lo_owned..hi_owned)
            .map(|guard| {
                guard
                    .into_inner()
                    .map(|(k, v)| (k.to_vec(), v.to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn prefix_scan<'a>(&'a self, prefix: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let prefix_owned = prefix.to_vec();
        let pairs: Vec<oxistore_core::RangeItem> = self
            .keyspace
            .prefix(&prefix_owned)
            .map(|guard| {
                guard
                    .into_inner()
                    .map(|(k, v)| (k.to_vec(), v.to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn batch_write(&self, pairs: &[(&[u8], &[u8])]) -> Result<(), StoreError> {
        let mut batch = self.db.batch();
        for &(k, v) in pairs {
            batch.insert(&self.keyspace, k, v);
        }
        batch.commit().map_err(|e| StoreError::Other(e.to_string()))
    }

    fn batch_delete(&self, keys: &[&[u8]]) -> Result<(), StoreError> {
        let mut batch = self.db.batch();
        for &k in keys {
            batch.remove(&self.keyspace, k);
        }
        batch.commit().map_err(|e| StoreError::Other(e.to_string()))
    }

    fn count(&self) -> Result<u64, StoreError> {
        let n = self
            .keyspace
            .len()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(n as u64)
    }

    fn size_on_disk(&self) -> Result<u64, StoreError> {
        dir_size(&self.path).map_err(|e| StoreError::Io(Arc::new(e)))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let pairs: Vec<oxistore_core::RangeItem> = self
            .keyspace
            .iter()
            .map(|guard| {
                guard
                    .into_inner()
                    .map(|(k, v)| (k.to_vec(), v.to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn keys<'a>(&'a self) -> Result<KeysIter<'a>, StoreError> {
        let keys: Vec<Result<Vec<u8>, StoreError>> = self
            .keyspace
            .iter()
            .map(|guard| {
                guard
                    .into_inner()
                    .map(|(k, _v)| k.to_vec())
                    .map_err(|e| StoreError::Other(e.to_string()))
            })
            .collect();
        Ok(Box::new(keys.into_iter()))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Ok(Box::new(FjallTxn {
            batch: Some(self.db.batch()),
            keyspace: &self.keyspace,
            overlay: BTreeMap::new(),
            _lock: self
                .txn_lock
                .lock()
                .map_err(|e| StoreError::Other(format!("lock poisoned: {e}")))?,
        }))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Ok(Box::new(FjallSnap {
            snap: self.db.snapshot(),
            keyspace: &self.keyspace,
        }))
    }

    /// Flush the write journal to durable storage.
    ///
    /// This implementation calls [`fjall::Database::persist`] with
    /// [`PersistMode::SyncAll`], issuing a full fsync so that all previously
    /// written data survives a crash.  This is equivalent to calling
    /// [`FjallStore::persist_sync`] directly.
    ///
    /// Callers that want a best-effort flush without a full fsync should use
    /// `db.persist(PersistMode::Buffer)` via [`FjallStore::raw_snapshot`] or
    /// open the database with `manual_journal_persist = false` (the default),
    /// which flushes on each commit automatically.
    fn flush(&self) -> Result<(), StoreError> {
        self.db
            .persist(PersistMode::SyncAll)
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn put_with_ttl(
        &self,
        key: &[u8],
        value: &[u8],
        ttl: std::time::Duration,
    ) -> Result<(), StoreError> {
        let expiry = expiry_epoch_millis(ttl)?;
        let mut batch = self.db.batch();
        batch.insert(&self.keyspace, key, value);
        batch.insert(&self.ttl_keyspace, key, encode_expiry(expiry).as_ref());
        batch.commit().map_err(|e| StoreError::Other(e.to_string()))
    }

    fn expire(&self, key: &[u8], ttl: std::time::Duration) -> Result<(), StoreError> {
        let exists = self
            .keyspace
            .get(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
            .is_some();
        if !exists {
            return Err(StoreError::KeyNotFound);
        }
        let expiry = expiry_epoch_millis(ttl)?;
        self.ttl_keyspace
            .insert(key, encode_expiry(expiry).as_ref())
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn ttl(&self, key: &[u8]) -> Result<Option<std::time::Duration>, StoreError> {
        let data_exists = self
            .keyspace
            .get(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
            .is_some();
        if !data_exists {
            return Err(StoreError::KeyNotFound);
        }
        match self
            .ttl_keyspace
            .get(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
        {
            None => Ok(None),
            Some(expiry_bytes) => {
                let expiry_millis = decode_expiry(&expiry_bytes)
                    .ok_or_else(|| StoreError::Other("invalid TTL encoding".to_string()))?;
                if is_expired(expiry_millis) {
                    let mut batch = self.db.batch();
                    batch.remove(&self.keyspace, key);
                    batch.remove(&self.ttl_keyspace, key);
                    batch
                        .commit()
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
            .keyspace
            .get(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
            .is_some();
        if !data_exists {
            return Err(StoreError::KeyNotFound);
        }
        let had_ttl = self
            .ttl_keyspace
            .get(key)
            .map_err(|e| StoreError::Other(e.to_string()))?
            .is_some();
        if had_ttl {
            self.ttl_keyspace
                .remove(key)
                .map_err(|e| StoreError::Other(e.to_string()))?;
        }
        Ok(had_ttl)
    }

    fn purge_expired(&self) -> Result<u64, StoreError> {
        // Collect expired keys first.
        let expired_keys: Vec<Vec<u8>> = self
            .ttl_keyspace
            .iter()
            .filter_map(|guard| {
                guard.into_inner().ok().and_then(|(k, v)| {
                    decode_expiry(&v).and_then(|expiry_millis| {
                        if is_expired(expiry_millis) {
                            Some(k.to_vec())
                        } else {
                            None
                        }
                    })
                })
            })
            .collect();

        if expired_keys.is_empty() {
            return Ok(0);
        }

        let count = expired_keys.len() as u64;
        let mut batch = self.db.batch();
        for key in &expired_keys {
            batch.remove(&self.keyspace, key.as_slice());
            batch.remove(&self.ttl_keyspace, key.as_slice());
        }
        batch
            .commit()
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(count)
    }
}

// ------------------------------------------------------------------
// FjallTxn — with read-your-writes overlay
// ------------------------------------------------------------------

/// Buffered operation within a transaction overlay.
#[derive(Clone)]
enum TxnOp {
    /// Value was inserted/updated.
    Put(Vec<u8>),
    /// Key was deleted.
    Delete,
}

/// A buffered write transaction over a [`fjall::OwnedWriteBatch`].
///
/// Supports **read-your-writes**: reads consult the local overlay of
/// buffered puts/deletes before falling back to committed state.
pub struct FjallTxn<'a> {
    /// The write batch; `None` after commit or rollback.
    batch: Option<fjall::OwnedWriteBatch>,
    keyspace: &'a Keyspace,
    /// Local overlay for read-your-writes.
    overlay: BTreeMap<Vec<u8>, TxnOp>,
    /// Held to serialise concurrent transactions.
    _lock: std::sync::MutexGuard<'a, ()>,
}

impl KvTxn for FjallTxn<'_> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        // Check overlay first (read-your-writes).
        if let Some(op) = self.overlay.get(key) {
            return match op {
                TxnOp::Put(v) => Ok(Some(v.clone())),
                TxnOp::Delete => Ok(None),
            };
        }
        // Fall through to committed state.
        self.keyspace
            .get(key)
            .map(|opt| opt.map(|v| v.to_vec()))
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let batch = self
            .batch
            .as_mut()
            .ok_or_else(|| StoreError::Other("transaction already consumed".to_string()))?;
        batch.insert(self.keyspace, key, value);
        self.overlay
            .insert(key.to_vec(), TxnOp::Put(value.to_vec()));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StoreError> {
        let batch = self
            .batch
            .as_mut()
            .ok_or_else(|| StoreError::Other("transaction already consumed".to_string()))?;
        batch.remove(self.keyspace, key);
        self.overlay.insert(key.to_vec(), TxnOp::Delete);
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
        for guard in self.keyspace.range(lo_owned.clone()..hi_owned.clone()) {
            match guard.into_inner() {
                Ok((k, v)) => {
                    merged.insert(k.to_vec(), v.to_vec());
                }
                Err(e) => return Err(StoreError::Other(e.to_string())),
            }
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

    fn commit(mut self: Box<Self>) -> Result<(), StoreError> {
        self.batch
            .take()
            .ok_or_else(|| StoreError::Other("transaction already consumed".to_string()))?
            .commit()
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    fn rollback(mut self: Box<Self>) -> Result<(), StoreError> {
        // Simply drop the batch without committing.
        self.batch.take();
        Ok(())
    }
}

// ------------------------------------------------------------------
// FjallSnap
// ------------------------------------------------------------------

/// A point-in-time read-only snapshot backed by [`fjall::Snapshot`].
///
/// The snapshot reflects the state of the database at the moment
/// [`KvStore::snapshot`] was called.
pub struct FjallSnap<'a> {
    snap: fjall::Snapshot,
    keyspace: &'a Keyspace,
}

impl KvSnapshot for FjallSnap<'_> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.snap
            .get(self.keyspace, key)
            .map(|opt| opt.map(|v| v.to_vec()))
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    /// Perform a range scan on this snapshot without eagerly collecting results.
    ///
    /// The returned iterator is **lazy**: rows are decoded one at a time as the
    /// caller advances the iterator, rather than materialising all matching
    /// rows into a `Vec` upfront.  This is significantly more memory-efficient
    /// for wide scans over large keyspaces.
    ///
    /// The snapshot is kept alive for the iterator's lifetime via the
    /// underlying `fjall::Iter`, which holds a reference to the snapshot
    /// nonce.
    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let lo_owned = lo.to_vec();
        let hi_owned = hi.to_vec();
        // `fjall::Iter` is `Send + 'static`, so we can box it as `RangeIter<'a>`
        // without any lifetime trouble — the snapshot nonce is held inside the
        // `fjall::Iter` itself, keeping the GC watermark in place.
        let iter = self
            .snap
            .range(self.keyspace, lo_owned..hi_owned)
            .map(|guard| {
                guard
                    .into_inner()
                    .map(|(k, v)| (k.to_vec(), v.to_vec()))
                    .map_err(|e| StoreError::Other(e.to_string()))
            });
        Ok(Box::new(iter))
    }
}

// ------------------------------------------------------------------
// FjallStoreBuilder
// ------------------------------------------------------------------

/// Builder for [`FjallStore`].
///
/// Provides fine-grained control over the underlying fjall database:
/// custom block-cache capacity, journal persistence mode, bloom filter
/// bits-per-key, compaction strategy, and compression type.
///
/// # Example
///
/// ```no_run
/// use oxistore_kv_fjall::{FjallStoreBuilder, FjallStore};
/// use fjall::PersistMode;
/// use oxistore_core::KvStore;
///
/// # let path = std::env::temp_dir().join("my-fjall-store");
/// let store = FjallStoreBuilder::new()
///     .block_cache_bytes(64 * 1024 * 1024)
///     .bloom_filter_bits_per_key(10.0)
///     .journal_persist_mode(PersistMode::SyncAll)
///     .build(&path)
///     .expect("build failed");
/// store.put(b"hello", b"world").expect("put failed");
/// ```
pub struct FjallStoreBuilder {
    block_cache_bytes: Option<u64>,
    journal_persist_mode: Option<PersistMode>,
    /// Bits-per-key for the bloom filter on all levels.  When set, overrides
    /// the fjall default (`10.0` for all non-last levels).
    bloom_filter_bits_per_key: Option<f32>,
    /// Name of the compaction strategy to apply to the default and TTL keyspaces.
    /// `None` means fjall's default (Leveled compaction).
    compaction_strategy_name: Option<CompactionStrategyKind>,
    /// Compression type to use for data blocks.
    ///
    /// When `None`, fjall's default (`Lz4` when the `lz4` feature is enabled,
    /// `None` otherwise) is used.  Set to [`CompressionType::None`] to
    /// disable compression entirely.
    data_block_compression: Option<CompressionType>,
}

/// Selects a named compaction strategy for [`FjallStoreBuilder`].
///
/// For more advanced strategies (e.g. FIFO with a size limit), call
/// [`KeyspaceCreateOptions::compaction_strategy`] directly.
#[non_exhaustive]
pub enum CompactionStrategyKind {
    /// Leveled compaction (default in fjall).
    Leveled,
}

impl Default for FjallStoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FjallStoreBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self {
            block_cache_bytes: None,
            journal_persist_mode: None,
            bloom_filter_bits_per_key: None,
            compaction_strategy_name: None,
            data_block_compression: None,
        }
    }

    /// Set the block cache capacity in bytes.
    ///
    /// It is recommended to set this to ~20–25 % of available memory when
    /// the data set does not fit entirely in cache.
    #[must_use]
    pub fn block_cache_bytes(mut self, n: u64) -> Self {
        self.block_cache_bytes = Some(n);
        self
    }

    /// Configure whether write batches automatically persist to durable
    /// storage or require a manual call to
    /// [`FjallStore::persist_sync`].
    ///
    /// When set to [`PersistMode::SyncAll`] or [`PersistMode::SyncData`],
    /// every write batch commit performs an fsync, giving full durability
    /// guarantees at the cost of higher write latency.
    #[must_use]
    pub fn journal_persist_mode(mut self, mode: PersistMode) -> Self {
        self.journal_persist_mode = Some(mode);
        self
    }

    /// Set the number of bloom filter bits per key applied to all SST levels.
    ///
    /// Higher values reduce false-positive rates (better point read performance)
    /// at the cost of additional memory and disk usage for filter blocks.
    /// Typical values range from `5.0` (coarse) to `20.0` (fine).
    ///
    /// When not set, fjall uses `10.0` bits per key as the default.
    #[must_use]
    pub fn bloom_filter_bits_per_key(mut self, bits: f32) -> Self {
        self.bloom_filter_bits_per_key = Some(bits);
        self
    }

    /// Select the compaction strategy to apply to the `"default"` and `"__ttl__"` keyspaces.
    ///
    /// When not called, fjall uses Leveled compaction by default.
    #[must_use]
    pub fn compaction_strategy_kind(mut self, kind: CompactionStrategyKind) -> Self {
        self.compaction_strategy_name = Some(kind);
        self
    }

    /// Configure the data-block compression type for the `"default"` and
    /// `"__ttl__"` keyspaces.
    ///
    /// fjall supports per-keyspace data-block compression via the
    /// `data_block_compression_policy` option.  Use this method to override
    /// the fjall default:
    ///
    /// | Feature flag | fjall default                         |
    /// |--------------|---------------------------------------|
    /// | `lz4` (on)   | L0+L1: `None`, L2+: `Lz4`            |
    /// | `lz4` (off)  | `None` (no compression)               |
    ///
    /// Setting [`CompressionType::None`] disables compression on all levels.
    /// Setting [`CompressionType::Lz4`] enables LZ4 on all levels.
    ///
    /// LZ4 is a pure-Rust port (`lz4_flex`) bundled with fjall — it does not
    /// introduce any C/FFI dependency and is compatible with the COOLJAPAN
    /// Pure Rust policy.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use oxistore_kv_fjall::FjallStoreBuilder;
    /// use fjall::CompressionType;
    ///
    /// # let path = std::env::temp_dir().join("no-compress");
    /// let store = FjallStoreBuilder::new()
    ///     .compression_type(CompressionType::None)  // disable compression
    ///     .build(&path)
    ///     .unwrap();
    /// ```
    #[must_use]
    pub fn compression_type(mut self, compression: CompressionType) -> Self {
        self.data_block_compression = Some(compression);
        self
    }

    /// Build a [`FjallStore`] at `path`.
    ///
    /// The directory is created automatically if it does not exist.
    pub fn build(self, path: impl AsRef<Path>) -> Result<FjallStore, FjallStoreError> {
        let path = path.as_ref();

        std::fs::create_dir_all(path).map_err(|e| FjallStoreError::Open(e.to_string()))?;

        let mut builder = Database::builder(path);
        if let Some(bytes) = self.block_cache_bytes {
            builder = builder.cache_size(bytes);
        }
        if let Some(mode) = self.journal_persist_mode {
            // manual_journal_persist = true means the caller controls when
            // to call db.persist().  For SyncAll/SyncData we set it to false
            // (automatic per-commit fsync).
            let manual = matches!(mode, PersistMode::Buffer);
            builder = builder.manual_journal_persist(manual);
        }

        let db = builder
            .open()
            .map_err(|e| FjallStoreError::Open(e.to_string()))?;

        // Build the keyspace options factory closure capturing our config.
        let bloom_bpk = self.bloom_filter_bits_per_key;
        let compaction_kind = self.compaction_strategy_name;
        let compression = self.data_block_compression;

        let make_options = move || {
            let mut opts = KeyspaceCreateOptions::default();
            if let Some(bits) = bloom_bpk {
                // Override all levels with the requested bits-per-key policy.
                let policy = FilterPolicy::all(FilterPolicyEntry::Bloom(
                    BloomConstructionPolicy::BitsPerKey(bits),
                ));
                opts = opts.filter_policy(policy);
            }
            if let Some(CompactionStrategyKind::Leveled) = compaction_kind {
                opts = opts.compaction_strategy(Arc::new(fjall::compaction::Leveled::default()));
            }
            // None → fjall's own default (Leveled) is used automatically.
            if let Some(comp) = compression {
                // Apply the requested compression to all levels uniformly.
                opts = opts.data_block_compression_policy(CompressionPolicy::all(comp));
            }
            opts
        };

        let keyspace = db
            .keyspace("default", &make_options)
            .map_err(|e| FjallStoreError::Open(e.to_string()))?;

        let ttl_keyspace = db
            .keyspace("__ttl__", make_options)
            .map_err(|e| FjallStoreError::Open(e.to_string()))?;

        Ok(FjallStore {
            db,
            keyspace,
            ttl_keyspace,
            path: path.to_path_buf(),
            txn_lock: Arc::new(Mutex::new(())),
        })
    }
}

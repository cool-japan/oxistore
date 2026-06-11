#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore` — Pure Rust storage facade for the COOLJAPAN ecosystem.
//!
//! This crate re-exports the core traits from [`oxistore_core`] and provides
//! the [`open`] / [`open_with`] / [`open_in_memory`] convenience functions
//! that return a `Box<dyn KvStore>` backed by the selected engine.
//!
//! # Default backend
//!
//! The `kv-redb` feature (enabled by default) selects [redb](https://crates.io/crates/redb) as the backing
//! store.  Pass `--features kv-sled` to use sled instead (or in addition).
//!
//! # Feature flags
//!
//! | Feature | Description |
//! |---------|-------------|
//! | `kv-redb` | redb backend (default) |
//! | `kv-sled` | sled backend |
//! | `kv-fjall` | fjall LSM-tree backend |
//! | `columnar` | Parquet/Arrow columnar storage |
//! | `cache` | LRU and ARC cache primitives |
//! | `blob` | Blob storage with local + memory backends |
//! | `encrypt` | Cell-level AEAD encryption decorator |
//!
//! # Example
//!
//! ```no_run
//! use oxistore::{open, KvStore};
//!
//! # let path = std::env::temp_dir().join("my-oxistore");
//! let store = open(&path).expect("open failed");
//! store.put(b"hello", b"world").expect("put failed");
//! let val = store.get(b"hello").expect("get failed");
//! assert_eq!(val.as_deref(), Some(b"world".as_ref()));
//! ```

use std::io::Read as _;
use std::path::Path;

// Re-export core types.
pub use oxistore_core::{
    expiry_epoch_millis, is_expired, prefix_upper_bound, BlobStore, BoxKvStore, ColumnarStore,
    KeysIter, KvSnapshot, KvStore, KvTxn, RangeItem, RangeIter, StoreConfig, StoreError,
    StoreMetrics,
};

/// Which backend engine to use when opening a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// redb -- the default, ACID-compliant copy-on-write B-tree database.
    Redb,
    /// sled -- an alternative pure-Rust embedded database.
    Sled,
    /// fjall -- a pure-Rust LSM-tree engine with built-in LZ4 compression.
    Fjall,
}

/// Unified backend type discriminant covering all storage engine types.
///
/// This enum extends [`StoreKind`] to cover not just KV backends but also
/// columnar, blob, and cache storage types.  Useful for runtime introspection
/// and logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// redb KV backend.
    KvRedb,
    /// sled KV backend.
    KvSled,
    /// fjall KV backend.
    KvFjall,
    /// Parquet/Arrow columnar storage.
    Columnar,
    /// Local-filesystem blob storage.
    BlobLocal,
    /// In-memory blob storage.
    BlobMemory,
    /// LRU/ARC cache wrapper over a KV store.
    Cache,
}

impl From<StoreKind> for Backend {
    fn from(kind: StoreKind) -> Self {
        match kind {
            StoreKind::Redb => Backend::KvRedb,
            StoreKind::Sled => Backend::KvSled,
            StoreKind::Fjall => Backend::KvFjall,
        }
    }
}

/// Open a [`KvStore`] at `path` using the default backend (redb).
///
/// # Errors
///
/// Returns [`StoreError`] if the path cannot be opened or if the `kv-redb`
/// feature is not enabled.
#[must_use = "the store must be used; dropping it immediately is likely a bug"]
pub fn open(path: impl AsRef<Path>) -> Result<BoxKvStore, StoreError> {
    open_with(StoreKind::Redb, path)
}

/// Open a [`KvStore`] at `path` using the specified `kind`.
///
/// # Errors
///
/// Returns [`StoreError::Other`] if the requested backend feature is not
/// compiled in (e.g. `StoreKind::Sled` without the `kv-sled` feature).
#[must_use = "the store must be used; dropping it immediately is likely a bug"]
pub fn open_with(kind: StoreKind, path: impl AsRef<Path>) -> Result<BoxKvStore, StoreError> {
    open_with_inner(kind, path.as_ref())
}

/// Open an ephemeral in-memory [`KvStore`] for the specified backend.
///
/// # Supported backends
///
/// - `Redb`: uses `redb::backends::InMemoryBackend`.
/// - `Sled`: uses `sled::Config::temporary(true)`.
/// - `Fjall`: creates a temporary directory under `std::env::temp_dir()`.
///
/// # Errors
///
/// Returns [`StoreError::Other`] if the backend feature is not compiled in.
#[must_use = "the in-memory store must be used; dropping it immediately is likely a bug"]
pub fn open_in_memory(kind: StoreKind) -> Result<BoxKvStore, StoreError> {
    match kind {
        StoreKind::Redb => open_redb_in_memory(),
        StoreKind::Sled => open_sled_in_memory(),
        StoreKind::Fjall => {
            // fjall needs a directory; use a unique temp path.
            let dir = std::env::temp_dir().join(format!(
                "oxistore_fjall_mem_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            open_fjall(&dir)
        }
    }
}

fn open_with_inner(kind: StoreKind, path: &Path) -> Result<BoxKvStore, StoreError> {
    match kind {
        StoreKind::Redb => open_redb(path),
        StoreKind::Sled => open_sled(path),
        StoreKind::Fjall => open_fjall(path),
    }
}

#[cfg(feature = "kv-redb")]
fn open_redb(path: &Path) -> Result<BoxKvStore, StoreError> {
    oxistore_kv_redb::RedbStore::open(path).map(|s| Box::new(s) as BoxKvStore)
}

#[cfg(not(feature = "kv-redb"))]
fn open_redb(_path: &Path) -> Result<BoxKvStore, StoreError> {
    Err(StoreError::Other("kv-redb feature not enabled".to_string()))
}

#[cfg(feature = "kv-redb")]
fn open_redb_in_memory() -> Result<BoxKvStore, StoreError> {
    oxistore_kv_redb::RedbStore::open_in_memory().map(|s| Box::new(s) as BoxKvStore)
}

#[cfg(not(feature = "kv-redb"))]
fn open_redb_in_memory() -> Result<BoxKvStore, StoreError> {
    Err(StoreError::Other("kv-redb feature not enabled".to_string()))
}

#[cfg(feature = "kv-sled")]
fn open_sled(path: &Path) -> Result<BoxKvStore, StoreError> {
    oxistore_kv_sled::SledStore::open(path).map(|s| Box::new(s) as BoxKvStore)
}

#[cfg(not(feature = "kv-sled"))]
fn open_sled(_path: &Path) -> Result<BoxKvStore, StoreError> {
    Err(StoreError::Other("kv-sled feature not enabled".to_string()))
}

#[cfg(feature = "kv-sled")]
fn open_sled_in_memory() -> Result<BoxKvStore, StoreError> {
    oxistore_kv_sled::SledStore::open_temporary().map(|s| Box::new(s) as BoxKvStore)
}

#[cfg(not(feature = "kv-sled"))]
fn open_sled_in_memory() -> Result<BoxKvStore, StoreError> {
    Err(StoreError::Other("kv-sled feature not enabled".to_string()))
}

#[cfg(feature = "kv-fjall")]
fn open_fjall(path: &Path) -> Result<BoxKvStore, StoreError> {
    oxistore_kv_fjall::FjallStore::open(path)
        .map(|s| Box::new(s) as BoxKvStore)
        .map_err(|e| StoreError::Other(e.to_string()))
}

#[cfg(not(feature = "kv-fjall"))]
fn open_fjall(_path: &Path) -> Result<BoxKvStore, StoreError> {
    Err(StoreError::Other(
        "kv-fjall feature not enabled".to_string(),
    ))
}

/// Re-exports for the redb backend (available when the `kv-redb` feature is enabled).
#[cfg(feature = "kv-redb")]
pub mod kv_redb {
    /// B-tree backed [`oxistore_core::KvStore`] using redb.
    pub use oxistore_kv_redb::RedbStore;
}

/// Re-exports for the sled backend (available when the `kv-sled` feature is enabled).
#[cfg(feature = "kv-sled")]
pub mod kv_sled {
    /// Sled-backed [`oxistore_core::KvStore`].
    pub use oxistore_kv_sled::SledStore;
}

/// Re-exports for the fjall backend (available when the `kv-fjall` feature is enabled).
#[cfg(feature = "kv-fjall")]
pub mod kv_fjall {
    /// LSM-tree backed [`oxistore_core::KvStore`] using fjall.
    pub use oxistore_kv_fjall::FjallStore;
}

/// Columnar (Parquet / Arrow) storage (available when the `columnar` feature is enabled).
#[cfg(feature = "columnar")]
pub mod columnar {
    pub use oxistore_columnar::*;
}

/// Cache eviction primitives -- LRU and ARC (available when the `cache` feature is enabled).
#[cfg(feature = "cache")]
pub mod cache {
    pub use oxistore_cache::*;
}

/// Blob storage -- `BlobStore` trait with local-filesystem and in-memory backends
/// (available when the `blob` feature is enabled).
#[cfg(feature = "blob")]
pub mod blob {
    pub use oxistore_blob::{
        sha256, sha256_streaming, BlobError, BlobMeta, BlobStore, BlobStoreBuilder, ChunkedUpload,
        Digest, LocalBlobStore, MemoryBlobStore,
    };
}

/// Cell-level AEAD encryption decorator and key provider abstractions
/// (available when the `encrypt` feature is enabled).
#[cfg(feature = "encrypt")]
pub mod encrypt {
    pub use oxistore_encrypt::*;
}

/// OxiARC codec bridge for compression (available when the `compress` feature is enabled).
#[cfg(feature = "compress")]
pub mod compress {
    pub use oxistore_compress::{CompressError, OxiArcCodec};
}

// ── redb magic number: [b'r', b'e', b'd', b'b', 0x1A, 0x0A, 0xA9, 0x0D, 0x0A]
// Source: redb-4.1.0/src/tree_store/page_store/header.rs MAGICNUMBER constant.
const REDB_MAGIC: [u8; 9] = [b'r', b'e', b'd', b'b', 0x1A, 0x0A, 0xA9, 0x0D, 0x0A];

/// Attempt to detect which backend engine created the store at `path`.
///
/// - File whose first 9 bytes match the redb magic header → [`StoreKind::Redb`]
/// - Directory containing a `conf` file → [`StoreKind::Sled`]
/// - Directory without a `conf` file → [`StoreKind::Fjall`]
/// - Non-existent or unrecognized → error
///
/// # Errors
///
/// Returns [`StoreError`] if the path does not exist, cannot be read, or its
/// format is not recognized.
#[must_use = "the detected backend kind should be used; ignoring it is likely a mistake"]
pub fn detect_backend(path: impl AsRef<Path>) -> Result<StoreKind, StoreError> {
    let path = path.as_ref();
    if path.is_file() {
        let mut buf = [0u8; 9];
        let mut file =
            std::fs::File::open(path).map_err(|e| StoreError::Io(std::sync::Arc::new(e)))?;
        let n = file
            .read(&mut buf)
            .map_err(|e| StoreError::Io(std::sync::Arc::new(e)))?;
        if n >= REDB_MAGIC.len() && buf == REDB_MAGIC {
            return Ok(StoreKind::Redb);
        }
        return Err(StoreError::Other(format!(
            "unrecognized file format at {}",
            path.display()
        )));
    }
    if path.is_dir() {
        if path.join("conf").exists() {
            return Ok(StoreKind::Sled);
        }
        return Ok(StoreKind::Fjall);
    }
    Err(StoreError::Other(format!(
        "path does not exist: {}",
        path.display()
    )))
}

/// Destroy (remove) the store at `path` for the given `kind`.
///
/// - [`StoreKind::Redb`]: removes the single database file.
/// - [`StoreKind::Sled`] / [`StoreKind::Fjall`]: removes the entire directory tree.
///
/// No-op if the path does not exist.
///
/// # Errors
///
/// Returns [`StoreError`] if the file system operation fails.
pub fn destroy(kind: StoreKind, path: impl AsRef<Path>) -> Result<(), StoreError> {
    let path = path.as_ref();
    match kind {
        StoreKind::Redb => {
            if path.exists() && path.is_file() {
                std::fs::remove_file(path).map_err(|e| StoreError::Io(std::sync::Arc::new(e)))?;
            }
        }
        StoreKind::Sled | StoreKind::Fjall => {
            if path.exists() && path.is_dir() {
                std::fs::remove_dir_all(path)
                    .map_err(|e| StoreError::Io(std::sync::Arc::new(e)))?;
            }
        }
    }
    Ok(())
}

/// Create a backup of the store at `src` (opened with `kind`) to `dst`.
///
/// Delegates to the underlying backend's [`KvStore::backup`] method.
///
/// # Errors
///
/// Returns [`StoreError`] if the store cannot be opened or if the backend does
/// not support backup.
pub fn backup_store(
    kind: StoreKind,
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
) -> Result<(), StoreError> {
    let store = open_with(kind, src)?;
    store.backup(dst.as_ref())
}

/// Restore a store at `dst` from a backup at `backup_path` using `kind`.
///
/// Delegates to the underlying backend's [`KvStore::restore`] method.
///
/// # Errors
///
/// Returns [`StoreError`] if the store cannot be opened or if the backend does
/// not support restore.
pub fn restore_store(
    kind: StoreKind,
    backup_path: impl AsRef<Path>,
    dst: impl AsRef<Path>,
) -> Result<(), StoreError> {
    let store = open_with(kind, dst)?;
    store.restore(backup_path.as_ref())
}

/// Open a local filesystem blob store rooted at `path`.
///
/// The directory is created lazily on the first `put` call.
///
/// # Errors
///
/// Returns [`StoreError`] if the path is otherwise invalid (the actual
/// filesystem check is deferred to the first write operation).
#[must_use = "the blob store must be used; dropping it immediately is likely a bug"]
#[cfg(feature = "blob")]
pub fn open_blob(path: impl AsRef<Path>) -> Result<oxistore_blob::LocalBlobStore, StoreError> {
    Ok(oxistore_blob::LocalBlobStore::new(path.as_ref()))
}

/// Open an existing Parquet columnar table at `path`.
///
/// Reads all row groups from a previously written Parquet file and returns a
/// [`oxistore_columnar::ColumnarTable`].  To create a new columnar table from
/// scratch, construct [`oxistore_columnar::ColumnarTable::new`] with an
/// appropriate schema and use [`oxistore_columnar::ColumnarTable::write_to`]
/// to persist it.
///
/// # Errors
///
/// Returns [`StoreError::Other`] if the file cannot be opened or is not a
/// valid Parquet file.
#[must_use = "the columnar table must be used; dropping it immediately is likely a bug"]
#[cfg(feature = "columnar")]
pub fn open_columnar(
    path: impl AsRef<Path>,
) -> Result<oxistore_columnar::ColumnarTable, StoreError> {
    oxistore_columnar::ColumnarTable::read_from(path.as_ref())
        .map_err(|e| StoreError::Other(e.to_string()))
}

/// Type alias for a KV store wrapped in a read-through LRU cache.
///
/// Produced by [`open_cached`].
#[cfg(all(
    feature = "cache",
    any(feature = "kv-redb", feature = "kv-sled", feature = "kv-fjall")
))]
pub type CachedKvStore =
    oxistore_cache::CacheableKvStore<BoxStoreAdapter, oxistore_cache::LruCache<Vec<u8>, Vec<u8>>>;

/// Open a [`KvStore`] at `path` and wrap it in a read-through LRU cache.
///
/// Each cache hit avoids a round-trip to the underlying storage engine.  The
/// cache holds up to `cache_cap` entries; once full, the least-recently-used
/// entry is evicted.
///
/// # Errors
///
/// Returns [`StoreError`] if the backing store cannot be opened or if the
/// required feature flags are not compiled in.
#[must_use = "the cached store must be used; dropping it immediately is likely a bug"]
#[cfg(all(
    feature = "cache",
    any(feature = "kv-redb", feature = "kv-sled", feature = "kv-fjall")
))]
pub fn open_cached(
    kind: StoreKind,
    path: impl AsRef<Path>,
    cache_cap: usize,
) -> Result<CachedKvStore, StoreError> {
    let inner = open_with_inner(kind, path.as_ref())?;
    let adapter = BoxStoreAdapter { inner };
    let cache = oxistore_cache::LruCache::new(cache_cap);
    Ok(oxistore_cache::CacheableKvStore::new(adapter, cache))
}

/// Open a [`KvStore`] at `path` with a [`StoreConfig`] using the default backend (redb).
///
/// The config's `read_only` field is respected: when `true`, any attempt to
/// write through the returned store returns [`StoreError::ReadOnly`].
///
/// # Errors
///
/// Returns [`StoreError`] if the path cannot be opened, or if the `kv-redb`
/// feature is not enabled.
#[must_use = "the store must be used; dropping it immediately is likely a bug"]
pub fn open_config(path: impl AsRef<Path>, config: StoreConfig) -> Result<BoxKvStore, StoreError> {
    open_config_inner(path.as_ref(), config)
}

fn open_config_inner(path: &Path, config: StoreConfig) -> Result<BoxKvStore, StoreError> {
    if config.read_only {
        open_read_only_inner(path)
    } else {
        // Open with the default backend (redb).  Additional config fields
        // (cache_size_bytes, sync_writes) are hints; the redb backend uses
        // its defaults for fields it does not yet expose through StoreConfig.
        open_redb(path)
    }
}

/// Open an existing [`KvStore`] at `path` in read-only mode (default backend: redb).
///
/// A read-only store allows concurrent readers without acquiring write locks.
/// Any call to [`KvStore::put`], [`KvStore::delete`], or [`KvStore::transaction`]
/// on the returned store returns [`StoreError::ReadOnly`].
///
/// # Errors
///
/// Returns [`StoreError`] if the path does not exist or cannot be opened,
/// or if the `kv-redb` feature is not enabled.
#[must_use = "the store must be used; dropping it immediately is likely a bug"]
pub fn open_read_only(path: impl AsRef<Path>) -> Result<BoxKvStore, StoreError> {
    open_read_only_inner(path.as_ref())
}

#[cfg(feature = "kv-redb")]
fn open_read_only_inner(path: &Path) -> Result<BoxKvStore, StoreError> {
    // redb does not yet expose a dedicated read-only open path through our
    // thin wrapper.  We open a regular store and wrap it in a read-only guard
    // so that any write attempt is rejected at the facade level.
    let inner = open_redb(path)?;
    Ok(Box::new(ReadOnlyStore { inner }) as BoxKvStore)
}

#[cfg(not(feature = "kv-redb"))]
fn open_read_only_inner(_path: &Path) -> Result<BoxKvStore, StoreError> {
    Err(StoreError::Other("kv-redb feature not enabled".to_string()))
}

// ── ReadOnlyStore ─────────────────────────────────────────────────────────────

/// A thin wrapper that forwards reads to an inner [`KvStore`] while rejecting
/// all write operations with [`StoreError::ReadOnly`].
struct ReadOnlyStore {
    inner: BoxKvStore,
}

impl KvStore for ReadOnlyStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(key)
    }

    fn put(&self, _key: &[u8], _value: &[u8]) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }

    fn delete(&self, _key: &[u8]) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        self.inner.contains(key)
    }

    fn range<'a>(
        &'a self,
        lo: &[u8],
        hi: &[u8],
    ) -> Result<oxistore_core::RangeIter<'a>, StoreError> {
        self.inner.range(lo, hi)
    }

    fn iter<'a>(&'a self) -> Result<oxistore_core::RangeIter<'a>, StoreError> {
        self.inner.iter()
    }

    fn transaction(&self) -> Result<Box<dyn oxistore_core::KvTxn + '_>, StoreError> {
        Err(StoreError::ReadOnly)
    }

    fn snapshot(&self) -> Result<Box<dyn oxistore_core::KvSnapshot + '_>, StoreError> {
        self.inner.snapshot()
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.inner.flush()
    }
}

// ── BoxStoreAdapter ───────────────────────────────────────────────────────────

/// A concrete wrapper around a [`BoxKvStore`] that implements [`KvStore`].
///
/// `BoxKvStore` is `Box<dyn KvStore>` and does not itself implement `KvStore`
/// (no blanket impl exists).  This newtype bridges the gap so that
/// `CacheableKvStore<BoxStoreAdapter, C>` can be constructed from any dynamically
/// dispatched store returned by `open_with`.
#[cfg(all(
    feature = "cache",
    any(feature = "kv-redb", feature = "kv-sled", feature = "kv-fjall")
))]
pub struct BoxStoreAdapter {
    inner: BoxKvStore,
}

#[cfg(all(
    feature = "cache",
    any(feature = "kv-redb", feature = "kv-sled", feature = "kv-fjall")
))]
impl KvStore for BoxStoreAdapter {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(key)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.inner.put(key, value)
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.inner.delete(key)
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        self.inner.contains(key)
    }

    fn range<'a>(
        &'a self,
        lo: &[u8],
        hi: &[u8],
    ) -> Result<oxistore_core::RangeIter<'a>, StoreError> {
        self.inner.range(lo, hi)
    }

    fn iter<'a>(&'a self) -> Result<oxistore_core::RangeIter<'a>, StoreError> {
        self.inner.iter()
    }

    fn transaction(&self) -> Result<Box<dyn oxistore_core::KvTxn + '_>, StoreError> {
        self.inner.transaction()
    }

    fn snapshot(&self) -> Result<Box<dyn oxistore_core::KvSnapshot + '_>, StoreError> {
        self.inner.snapshot()
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.inner.flush()
    }
}

/// Typed KV adapter re-exports (available when the `serde-typed` feature is enabled).
#[cfg(feature = "serde-typed")]
pub use oxistore_core::{JsonCodec, TypedCodec, TypedKvError, TypedKvStore};

// ── Prelude ────────────────────────────────────────────────────────────────────

/// Prelude module — import commonly used types with `use oxistore::prelude::*`.
///
/// # Example
///
/// ```no_run
/// use oxistore::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{open, open_in_memory, open_with, Backend, StoreError, StoreKind};
    pub use oxistore_core::{KvSnapshot, KvStore, KvTxn};

    #[cfg(feature = "cache")]
    pub use oxistore_cache::{ArcCache, LruCache};

    #[cfg(feature = "blob")]
    pub use oxistore_blob::{BlobMeta, LocalBlobStore, MemoryBlobStore};

    #[cfg(feature = "columnar")]
    pub use oxistore_columnar::ColumnarTable;

    #[cfg(feature = "encrypt")]
    pub use oxistore_encrypt::EncryptedKv;

    #[cfg(feature = "serde-typed")]
    pub use oxistore_core::{JsonCodec, TypedCodec, TypedKvError, TypedKvStore};
}

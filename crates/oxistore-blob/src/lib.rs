#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore-blob` — Pure Rust blob storage for the COOLJAPAN ecosystem.
//!
//! This crate provides the [`BlobStore`] trait and two Pure-Rust backend
//! implementations:
//!
//! - [`LocalBlobStore`] — filesystem-backed, with atomic put via temp-file rename.
//! - [`MemoryBlobStore`] — in-memory, backed by a `BTreeMap` under a `RwLock`.
//!
//! A cloud backend (`S3`, `Azure Blob Storage`, `GCS`) is **deferred** -- see
//! `src/cloud.rs` for the full blocker analysis.
//!
//! # Content-Addressed Storage
//!
//! The [`BlobStore`] trait provides CAS methods ([`BlobStore::put_cas`],
//! [`BlobStore::get_cas`], [`BlobStore::exists_cas`]) that automatically
//! compute a SHA-256 [`Digest`] and use it as the storage key.  Identical
//! content always maps to the same key, giving free deduplication.
//! [`BlobStore::get_cas`] re-verifies the SHA-256 on every read so
//! storage corruption is detected immediately.
//!
//! Streaming variants ([`BlobStore::put_streaming`], [`BlobStore::get_verified`])
//! work identically but accept or produce byte-stream inputs via
//! [`tokio::io::AsyncRead`].
//!
//! # Chunked Upload
//!
//! [`ChunkedUpload`] accumulates byte chunks and finalises them atomically via
//! [`BlobStore::put_chunked`].
//!
//! # Builder
//!
//! [`BlobStoreBuilder`] allows configuring a storage backend before
//! construction.  Use [`BlobStoreBuilder::build_memory`] to obtain a
//! capacity-limited in-memory store.
//!
//! # Example
//!
//! ```no_run
//! use oxistore_blob::{BlobStore, MemoryBlobStore};
//! use bytes::Bytes;
//!
//! # async fn example() -> Result<(), oxistore_blob::BlobError> {
//! let store = MemoryBlobStore::new();
//! store.put("readme.txt", Bytes::from("hello")).await?;
//! let data = store.get("readme.txt").await?;
//! assert_eq!(data.as_ref(), b"hello");
//! # Ok(())
//! # }
//! ```

/// Content-addressed storage primitives (SHA-256 digest, helpers).
pub mod cas;
/// Cloud blob backends (S3 / Azure / GCS) -- **deferred** (see module docs).
pub mod cloud;
/// Error types for blob storage operations.
pub mod error;
/// Filesystem-backed blob store implementation.
pub mod local;
/// In-memory blob store implementation backed by a `BTreeMap`.
pub mod memory;

pub use cas::{sha256, sha256_streaming, Digest};
pub use error::BlobError;
pub use local::LocalBlobStore;
pub use memory::MemoryBlobStore;

// ── Concrete impls: satisfy oxistore_core::BlobStore marker ─────────────────
//
// We cannot write `impl<T: BlobStore> oxistore_core::BlobStore for T {}` due to
// the orphan rule — `oxistore_core::BlobStore` is a foreign trait.
// Instead we implement the marker for each concrete type defined in this crate.

impl oxistore_core::BlobStore for LocalBlobStore {}
impl oxistore_core::BlobStore for MemoryBlobStore {}

use bytes::Bytes;
use std::future::Future;

/// Metadata for a blob entry.
///
/// Returned by [`BlobStore::head`], [`BlobStore::list_meta`], and
/// [`BlobStore::list_meta_page`].
///
/// This struct is marked `#[non_exhaustive]` so that new metadata fields can
/// be added in future minor versions without breaking downstream code.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    /// The key identifying this blob within the store.
    pub key: String,
    /// The size of the blob in bytes.
    pub size: u64,
    /// Optional MIME content type.
    ///
    /// Set by the caller at `put` time; `None` when the backend does not
    /// track or store content-type information.
    pub content_type: Option<String>,
    /// Optional SHA-256 checksum of the blob content.
    ///
    /// Present when the blob was stored via [`BlobStore::put_cas`] or
    /// when the backend explicitly computes and stores the digest.
    /// `None` for blobs stored via plain [`BlobStore::put`].
    pub checksum: Option<Digest>,
}

impl BlobMeta {
    /// Create a minimal `BlobMeta` with no content type or checksum.
    ///
    /// This is the primary constructor for users outside the `oxistore-blob`
    /// crate — required because `BlobMeta` is `#[non_exhaustive]`.
    #[must_use]
    pub fn new(key: impl Into<String>, size: u64) -> Self {
        Self {
            key: key.into(),
            size,
            content_type: None,
            checksum: None,
        }
    }
}

// ── ChunkedUpload ─────────────────────────────────────────────────────────────

/// Accumulates byte chunks and produces the assembled payload on demand.
///
/// Use [`BlobStore::put_chunked`] to atomically store the assembled content
/// under a key.
///
/// # Example
///
/// ```
/// use oxistore_blob::ChunkedUpload;
///
/// let mut upload = ChunkedUpload::new();
/// upload.push_chunk(b"hello, ".as_slice());
/// upload.push_chunk(b"world!".as_slice());
/// assert_eq!(upload.assemble(), b"hello, world!");
/// ```
#[derive(Debug, Default)]
pub struct ChunkedUpload {
    chunks: Vec<Vec<u8>>,
}

impl ChunkedUpload {
    /// Create a new, empty chunked upload session.
    pub fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    /// Append a chunk to this upload session.
    pub fn push_chunk(&mut self, chunk: impl Into<Vec<u8>>) {
        self.chunks.push(chunk.into());
    }

    /// Consume the session and return the assembled bytes (concatenation of all
    /// chunks in insertion order).
    pub fn assemble(self) -> Vec<u8> {
        let total = self.chunks.iter().map(|c| c.len()).sum();
        let mut out = Vec::with_capacity(total);
        for chunk in self.chunks {
            out.extend_from_slice(&chunk);
        }
        out
    }
}

// ── BlobStoreBuilder ──────────────────────────────────────────────────────────

/// Builder for configuring and constructing a [`BlobStore`] backend.
///
/// # Example
///
/// ```
/// use oxistore_blob::BlobStoreBuilder;
///
/// let store = BlobStoreBuilder::new()
///     .capacity_bytes(1024 * 1024)
///     .build_memory();
/// ```
#[derive(Debug, Default)]
pub struct BlobStoreBuilder {
    capacity_bytes: Option<u64>,
}

impl BlobStoreBuilder {
    /// Create a builder with default settings (no capacity limit).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set an upper bound on total stored bytes.
    ///
    /// When this limit is set, any [`BlobStore::put`] call that would exceed it
    /// returns [`BlobError::QuotaExceeded`].
    pub fn capacity_bytes(self, n: u64) -> Self {
        Self {
            capacity_bytes: Some(n),
        }
    }

    /// Build an in-memory [`MemoryBlobStore`] with the configured settings.
    pub fn build_memory(self) -> MemoryBlobStore {
        match self.capacity_bytes {
            Some(cap) => MemoryBlobStore::with_capacity(cap),
            None => MemoryBlobStore::new(),
        }
    }
}

// ── BlobStore trait ───────────────────────────────────────────────────────────

/// Core trait for blob storage backends.
///
/// A `BlobStore` maps string keys to opaque byte payloads.  Keys are
/// arbitrary non-empty UTF-8 strings; their exact namespace semantics (e.g.
/// `/`-separated path hierarchy) depend on the backend.
///
/// All operations are asynchronous.  Implementations must be `Send + Sync` so
/// they can be shared across tasks.
pub trait BlobStore: Send + Sync {
    /// Store `data` under `key`, overwriting any existing value.
    fn put(&self, key: &str, data: Bytes) -> impl Future<Output = Result<(), BlobError>> + Send;

    /// Retrieve the blob stored under `key`.
    ///
    /// Returns [`BlobError::NotFound`] if no blob exists for `key`.
    fn get(&self, key: &str) -> impl Future<Output = Result<Bytes, BlobError>> + Send;

    /// Remove the blob stored under `key`.
    ///
    /// Returns [`BlobError::NotFound`] if the key does not exist.
    fn delete(&self, key: &str) -> impl Future<Output = Result<(), BlobError>> + Send;

    /// Return metadata for the blob stored under `key` without fetching the
    /// full payload.
    ///
    /// Returns [`BlobError::NotFound`] if the key does not exist.
    fn head(&self, key: &str) -> impl Future<Output = Result<BlobMeta, BlobError>> + Send;

    /// List all keys that start with `prefix`, in ascending lexicographic order.
    ///
    /// Pass an empty string to list all keys.
    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, BlobError>> + Send;

    // ── Higher-level helpers (provided default implementations) ───────────────

    /// Return `true` if a blob exists under `key`.
    ///
    /// Default implementation delegates to [`BlobStore::head`].
    fn exists(&self, key: &str) -> impl Future<Output = Result<bool, BlobError>> + Send {
        let head_fut = self.head(key);
        async move {
            match head_fut.await {
                Ok(_) => Ok(true),
                Err(BlobError::NotFound(_)) => Ok(false),
                Err(e) => Err(e),
            }
        }
    }

    /// Copy a blob from `src_key` to `dst_key`.
    ///
    /// Default implementation reads the source and writes to the destination.
    fn copy(
        &self,
        src_key: &str,
        dst_key: &str,
    ) -> impl Future<Output = Result<(), BlobError>> + Send {
        let get_fut = self.get(src_key);
        let dst_owned = dst_key.to_string();
        async move {
            let data = get_fut.await?;
            self.put(&dst_owned, data).await
        }
    }

    /// Rename a blob from `old_key` to `new_key`.
    ///
    /// Default implementation copies then deletes.
    fn rename(
        &self,
        old_key: &str,
        new_key: &str,
    ) -> impl Future<Output = Result<(), BlobError>> + Send {
        let get_fut = self.get(old_key);
        let new_owned = new_key.to_string();
        let old_owned = old_key.to_string();
        async move {
            let data = get_fut.await?;
            self.put(&new_owned, data).await?;
            self.delete(&old_owned).await
        }
    }

    /// Delete multiple keys in a batch.
    ///
    /// Default implementation deletes keys sequentially.
    /// Keys that do not exist are silently skipped.
    fn delete_many(&self, keys: &[&str]) -> impl Future<Output = Result<(), BlobError>> + Send {
        let keys_owned: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        async move {
            for key in &keys_owned {
                match self.delete(key).await {
                    Ok(()) => {}
                    Err(BlobError::NotFound(_)) => {} // silently skip
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
    }

    /// Delete all keys matching the given `prefix`.
    ///
    /// Default implementation lists then deletes.
    fn delete_prefix(&self, prefix: &str) -> impl Future<Output = Result<u64, BlobError>> + Send {
        let list_fut = self.list(prefix);
        async move {
            let keys = list_fut.await?;
            let mut count = 0u64;
            for key in &keys {
                match self.delete(key).await {
                    Ok(()) => count += 1,
                    Err(BlobError::NotFound(_)) => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(count)
        }
    }

    /// Store `data` under `key` only if the key does not already exist.
    ///
    /// Returns `Err(BlobError::AlreadyExists)` if the key is taken.
    fn put_if_absent(
        &self,
        key: &str,
        data: Bytes,
    ) -> impl Future<Output = Result<(), BlobError>> + Send {
        let head_fut = self.head(key);
        let key_owned = key.to_string();
        async move {
            match head_fut.await {
                Ok(_) => Err(BlobError::AlreadyExists(key_owned)),
                Err(BlobError::NotFound(_)) => self.put(&key_owned, data).await,
                Err(e) => Err(e),
            }
        }
    }

    /// Assemble a [`ChunkedUpload`] and store it atomically under `key`.
    ///
    /// This is a convenience wrapper around [`BlobStore::put`].
    fn put_chunked(
        &self,
        key: &str,
        upload: ChunkedUpload,
    ) -> impl Future<Output = Result<(), BlobError>> + Send {
        let key_owned = key.to_string();
        let data = Bytes::from(upload.assemble());
        async move { self.put(&key_owned, data).await }
    }

    // ── list_meta / list_meta_page ────────────────────────────────────────────

    /// List metadata (key + size) for all blobs whose key starts with `prefix`.
    ///
    /// Default implementation calls [`BlobStore::list`] then [`BlobStore::head`]
    /// for each key.  Override for efficiency.
    fn list_meta(
        &self,
        prefix: &str,
    ) -> impl Future<Output = Result<Vec<BlobMeta>, BlobError>> + Send {
        let list_fut = self.list(prefix);
        async move {
            let keys = list_fut.await?;
            let mut metas = Vec::with_capacity(keys.len());
            for key in keys {
                let meta = self.head(&key).await?;
                metas.push(meta);
            }
            Ok(metas)
        }
    }

    /// List a page of metadata for blobs whose key starts with `prefix`.
    ///
    /// `start_after` is an exclusive lower-bound continuation token; pass
    /// `None` to start from the beginning.  `limit` caps the number of
    /// returned entries.
    ///
    /// Default implementation fetches the full `list_meta` result and then
    /// applies the cursor and limit in memory.  Override for efficiency.
    fn list_meta_page(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<BlobMeta>, BlobError>> + Send {
        let list_fut = self.list(prefix);
        let start_after_owned = start_after.map(str::to_string);
        async move {
            let keys = list_fut.await?;
            let mut metas = Vec::with_capacity(limit.min(keys.len()));
            for key in keys {
                if let Some(ref sa) = start_after_owned {
                    if key.as_str() <= sa.as_str() {
                        continue;
                    }
                }
                if metas.len() >= limit {
                    break;
                }
                let meta = self.head(&key).await?;
                metas.push(meta);
            }
            Ok(metas)
        }
    }

    // ── conditional-delete ────────────────────────────────────────────────────

    /// Delete `key` only if its content matches `digest`.
    ///
    /// Returns `Ok(true)` if the blob existed, its SHA-256 matched, and it was
    /// deleted.  Returns `Ok(false)` if the key does not exist or its digest
    /// does not match (the blob is left unchanged).  Returns `Err(...)` on I/O
    /// or other store errors.
    fn delete_if_matches(
        &self,
        key: &str,
        digest: &Digest,
    ) -> impl Future<Output = Result<bool, BlobError>> + Send {
        let key_owned = key.to_string();
        let expected = digest.clone();
        async move {
            let data = match self.get(&key_owned).await {
                Ok(d) => d,
                Err(BlobError::NotFound(_)) => return Ok(false),
                Err(e) => return Err(e),
            };
            let actual = crate::cas::sha256(&data);
            if actual != expected {
                return Ok(false);
            }
            self.delete(&key_owned).await?;
            Ok(true)
        }
    }

    // ── Content-addressed storage (CAS) ──────────────────────────────────────

    /// Store content at its SHA-256 digest address.
    ///
    /// Identical content always maps to the same [`Digest`] key.  If the
    /// digest already exists in the store, no second copy is written
    /// (deduplication).  Returns the SHA-256 [`Digest`] of the stored content.
    fn put_cas(&self, data: Bytes) -> impl Future<Output = Result<Digest, BlobError>> + Send {
        let digest = crate::cas::sha256(&data);
        let key = digest.to_hex();
        async move {
            match self.put_if_absent(&key, data).await {
                Ok(()) | Err(BlobError::AlreadyExists(_)) => Ok(digest),
                Err(e) => Err(e),
            }
        }
    }

    /// Retrieve content by its SHA-256 [`Digest`].
    ///
    /// Re-verifies the SHA-256 after reading; returns
    /// [`BlobError::ChecksumMismatch`] if the stored bytes have been corrupted.
    ///
    /// # Integrity
    ///
    /// The SHA-256 is recomputed after every read.  Storage corruption
    /// (including bit-rot and deliberate tampering) triggers
    /// [`BlobError::ChecksumMismatch`].
    fn get_cas(&self, digest: &Digest) -> impl Future<Output = Result<Bytes, BlobError>> + Send {
        let expected = digest.clone();
        let key = expected.to_hex();
        async move {
            let data = self.get(&key).await?;
            let actual = crate::cas::sha256(&data);
            if actual != expected {
                return Err(BlobError::ChecksumMismatch(format!(
                    "expected {expected}, got {actual}"
                )));
            }
            Ok(data)
        }
    }

    /// Return `true` if a blob with the given [`Digest`] exists in the store.
    fn exists_cas(&self, digest: &Digest) -> impl Future<Output = Result<bool, BlobError>> + Send {
        let key = digest.to_hex();
        async move {
            match self.head(&key).await {
                Ok(_) => Ok(true),
                Err(BlobError::NotFound(_)) => Ok(false),
                Err(e) => Err(e),
            }
        }
    }

    // ── Streaming CAS ─────────────────────────────────────────────────────────

    /// Stream content in from a [`tokio::io::AsyncRead`], hash and store.
    ///
    /// The entire payload is buffered in memory (streaming is used purely to
    /// compute the hash incrementally).  Returns the SHA-256 [`Digest`] of the
    /// stored content.
    fn put_streaming<R>(&self, reader: R) -> impl Future<Output = Result<Digest, BlobError>> + Send
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        use sha2::Digest as _;
        use tokio::io::AsyncReadExt;

        async move {
            let mut hasher = sha2::Sha256::new();
            let mut buf = [0u8; 65536];
            let mut reader = reader;
            let mut data = Vec::new();

            loop {
                let n = reader.read(&mut buf).await.map_err(BlobError::Io)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                data.extend_from_slice(&buf[..n]);
            }

            let hash = hasher.finalize();
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&hash);
            let digest = Digest::from_bytes(bytes);
            let key = digest.to_hex();

            match self.put_if_absent(&key, Bytes::from(data)).await {
                Ok(()) | Err(BlobError::AlreadyExists(_)) => Ok(digest),
                Err(e) => Err(e),
            }
        }
    }

    /// Retrieve a blob by its SHA-256 [`Digest`] with integrity verification.
    ///
    /// This is an alias for [`BlobStore::get_cas`] — it always verifies the
    /// checksum on every read.
    fn get_verified(
        &self,
        digest: &Digest,
    ) -> impl Future<Output = Result<Bytes, BlobError>> + Send {
        self.get_cas(digest)
    }
}

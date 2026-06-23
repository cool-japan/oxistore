# oxistore-blob — BlobStore trait with local-filesystem and in-memory backends

[![Crates.io](https://img.shields.io/crates/v/oxistore-blob.svg)](https://crates.io/crates/oxistore-blob)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-blob` defines the async [`BlobStore`] trait — the OxiStore abstraction for mapping string keys to opaque byte payloads — together with two Pure-Rust backends:

- **`LocalBlobStore`** — filesystem-backed, with atomic put via temp-file rename so readers never observe a partially-written blob.
- **`MemoryBlobStore`** — in-memory, backed by a `BTreeMap` under a `RwLock`, with an optional capacity quota.

The trait ships a rich set of **default methods** (copy, rename, batch/prefix delete, conditional put, paginated metadata listing, and content-addressed storage) so backends only need to implement five primitives — `put`, `get`, `delete`, `head`, `list` — to get the full surface. Cloud object-store backends are provided as separate crates that implement this same trait: [`oxistore-blob-s3`](../oxistore-blob-s3), [`oxistore-blob-gcs`](../oxistore-blob-gcs), and [`oxistore-blob-azure`](../oxistore-blob-azure). This crate is 100% Pure Rust (`bytes`, `tokio`, `sha2`) and forbids `unsafe` code.

## Installation

```toml
[dependencies]
oxistore-blob = "0.2.0"
```

## Quick Start

```rust
use oxistore_blob::{BlobStore, MemoryBlobStore};
use bytes::Bytes;

# async fn example() -> Result<(), oxistore_blob::BlobError> {
let store = MemoryBlobStore::new();
store.put("readme.txt", Bytes::from("hello")).await?;

let data = store.get("readme.txt").await?;
assert_eq!(data.as_ref(), b"hello");

let meta = store.head("readme.txt").await?;
assert_eq!(meta.size, 5);
# Ok(())
# }
```

### Content-addressed storage (CAS)

`put_cas` hashes content with SHA-256 and stores it at its digest address, giving free deduplication. `get_cas` re-verifies the SHA-256 on every read, so bit-rot or tampering surfaces as `BlobError::ChecksumMismatch`.

```rust
use oxistore_blob::{BlobStore, MemoryBlobStore};
use bytes::Bytes;

# async fn example() -> Result<(), oxistore_blob::BlobError> {
let store = MemoryBlobStore::new();
let digest = store.put_cas(Bytes::from("dedup me")).await?;
let same = store.put_cas(Bytes::from("dedup me")).await?; // no second copy
assert_eq!(digest, same);

let bytes = store.get_cas(&digest).await?; // checksum re-verified
assert_eq!(bytes.as_ref(), b"dedup me");
# Ok(())
# }
```

### Local filesystem backend

```rust
use oxistore_blob::{BlobStore, LocalBlobStore};
use bytes::Bytes;

# async fn example() -> Result<(), oxistore_blob::BlobError> {
let store = LocalBlobStore::new("/var/lib/myapp/blobs");
store.put("images/logo.png", Bytes::from_static(b"...")).await?;
let keys = store.list("images/").await?;
# Ok(())
# }
```

## API Overview

### `BlobStore` trait

`Send + Sync`; all operations are async. Keys are arbitrary non-empty UTF-8 strings.

**Required methods** (each backend implements these five):

| Method | Description |
|--------|-------------|
| `put(key, data)` | Store `data` under `key`, overwriting any existing value |
| `get(key)` | Retrieve the blob; `NotFound` if absent |
| `delete(key)` | Remove the blob; `NotFound` if absent |
| `head(key)` | Return `BlobMeta` without fetching the payload |
| `list(prefix)` | List keys starting with `prefix`, ascending; `""` lists all |

**Provided default methods:**

| Method | Description |
|--------|-------------|
| `exists(key)` | `true` if a blob exists (via `head`) |
| `copy(src, dst)` | Read source, write to destination |
| `rename(old, new)` | Copy then delete |
| `delete_many(keys)` | Delete a batch; missing keys silently skipped |
| `delete_prefix(prefix)` | Delete all keys under a prefix; returns the count |
| `put_if_absent(key, data)` | Store only if absent; `AlreadyExists` otherwise |
| `put_chunked(key, upload)` | Assemble a `ChunkedUpload` and store atomically |
| `list_meta(prefix)` | List `BlobMeta` for all keys under a prefix |
| `list_meta_page(prefix, start_after, limit)` | Paginated metadata with a continuation token |
| `delete_if_matches(key, digest)` | Delete only if content's SHA-256 matches; returns whether deleted |
| `put_cas(data)` | Store at SHA-256 address (deduplicating); returns the `Digest` |
| `get_cas(digest)` | Retrieve by digest, re-verifying the checksum |
| `exists_cas(digest)` | `true` if a blob with the given digest exists |
| `put_streaming(reader)` | Stream from `tokio::io::AsyncRead`, hash, and store; returns the `Digest` |
| `get_verified(digest)` | Alias for `get_cas` (always verifies) |

`LocalBlobStore` and `MemoryBlobStore` both override `list_meta` and `list_meta_page` for efficiency.

### `LocalBlobStore`

Filesystem-backed store. Keys map directly to paths under a base directory; `/`-separated keys create nested subdirectories. Keys must be non-empty and must not contain a `..` path component (directory-traversal protection — rejected with `BlobError::Other`).

| Constructor | Description |
|-------------|-------------|
| `LocalBlobStore::new(base)` | New store rooted at `base` (created lazily on first put) |
| `LocalBlobStore::with_checksum_verification(base)` | Like `new`, but every `get` re-verifies SHA-256 when the key is a 64-char hex digest |

Implements `Debug` and `Clone`.

### `MemoryBlobStore`

In-memory store backed by `Arc<RwLock<BTreeMap<String, Bytes>>>`; cloning shares the same data. Intended for tests and local development — it does not persist across restarts.

| Constructor | Description |
|-------------|-------------|
| `MemoryBlobStore::new()` | Empty store, no capacity limit |
| `MemoryBlobStore::default()` | Same as `new()` |

To configure a capacity quota, use `BlobStoreBuilder` (below). Implements `Debug` and `Clone`.

### `BlobMeta`

`#[non_exhaustive]` metadata struct returned by `head`, `list_meta`, and `list_meta_page`.

| Field | Type | Description |
|-------|------|-------------|
| `key` | `String` | The blob's key |
| `size` | `u64` | Size in bytes |
| `content_type` | `Option<String>` | MIME type, when tracked |
| `checksum` | `Option<Digest>` | SHA-256, present for CAS-stored blobs |

Construct via `BlobMeta::new(key, size)` (required because the struct is `#[non_exhaustive]`).

### `ChunkedUpload`

Accumulates byte chunks for atomic storage via `BlobStore::put_chunked`.

| Method | Description |
|--------|-------------|
| `ChunkedUpload::new()` | New empty session |
| `push_chunk(chunk)` | Append a chunk (`impl Into<Vec<u8>>`) |
| `assemble()` | Consume and return the concatenated bytes |

### `BlobStoreBuilder`

Builder for constructing a configured backend.

| Method | Description |
|--------|-------------|
| `BlobStoreBuilder::new()` | New builder (no capacity limit) |
| `capacity_bytes(n)` | Upper bound on total stored bytes (`put` returns `QuotaExceeded` past the limit) |
| `build_memory()` | Build a `MemoryBlobStore` with the configured settings |

### Content-addressed storage primitives (`cas` module)

| Item | Description |
|------|-------------|
| `Digest` | 32-byte SHA-256 content address; `Clone`, `PartialEq`, `Eq`, `Hash`, `Debug`, `Display` |
| `Digest::from_bytes([u8; 32])` | Construct from a raw array |
| `Digest::as_bytes()` | Borrow the underlying 32-byte array |
| `Digest::to_hex()` | Lower-case 64-character hex string |
| `Digest::from_hex(s)` | Decode a 64-char hex string; `BlobError::Other` on invalid input |
| `sha256(data)` | One-shot SHA-256 of a byte slice |
| `sha256_streaming(reader)` | SHA-256 of a `std::io::Read` stream until EOF |

### `BlobError` variants

`#[non_exhaustive]`. Implements `Display`, `Error` (with `source` for `Io`), and `From<std::io::Error>`.

| Variant | Description |
|---------|-------------|
| `NotFound(String)` | Key not found |
| `AlreadyExists(String)` | Key already exists (conditional put) |
| `Io(std::io::Error)` | Filesystem-level I/O error |
| `ChecksumMismatch(String)` | A checksum verification failed |
| `Other(String)` | Any other store-specific error |
| `QuotaExceeded { limit_bytes, needed_bytes }` | Operation would exceed the configured quota |
| `MultipartError(String)` | A multipart upload operation failed (used by cloud backends) |
| `RetryExhausted { attempts, last_error }` | All retry attempts exhausted (used by cloud backends) |

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `blob` | off | Marker feature for facade gating; the trait and backends are always built |

> Note: the `cloud` module in this crate's source documents an early `object_store`-based design that was **deferred** for Pure-Rust reasons. The shipping cloud backends are the standalone `oxistore-blob-s3`/`-gcs`/`-azure` crates, which implement `BlobStore` directly without `ring` on normal dependency edges.

## Cross-references

- [`oxistore-core`](../oxistore-core) — defines `KvStore`/`KvTxn`/`KvSnapshot` and the `BlobStore` marker trait re-exported by the facade.
- [`oxistore`](../oxistore) — facade; re-exports `BlobStore`, `LocalBlobStore`, `MemoryBlobStore`, etc. under `oxistore::blob` (with the `blob` feature) and offers `open_blob(path)`.
- [`oxistore-blob-s3`](../oxistore-blob-s3) / [`oxistore-blob-gcs`](../oxistore-blob-gcs) / [`oxistore-blob-azure`](../oxistore-blob-azure) — Pure-Rust cloud `BlobStore` backends.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

# oxistore — The COOLJAPAN Pure-Rust storage facade

[![Crates.io](https://img.shields.io/crates/v/oxistore.svg)](https://crates.io/crates/oxistore)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore` is the top-level facade crate for the OxiStore storage stack. It re-exports the core traits from [`oxistore-core`](../oxistore-core) and provides a small set of `open*` functions that return a `Box<dyn KvStore>` backed by a feature-selected engine — so callers depend on one crate and one trait, and swap storage engines by flipping a Cargo feature. Every backend is 100% Pure Rust: no C/C++/Fortran libraries are required.

The facade also gathers the optional OxiStore layers — columnar (Parquet/Arrow), blob, cache, encryption, and compression — behind feature flags, re-exporting each behind a dedicated module so the whole stack is reachable from a single dependency.

## Installation

```toml
[dependencies]
# Default: redb B-tree backend
oxistore = "0.2.0"

# sled backend
oxistore = { version = "0.2.0", default-features = false, features = ["kv-sled"] }

# fjall LSM-tree backend
oxistore = { version = "0.2.0", default-features = false, features = ["kv-fjall"] }

# All KV backends + cache + blob + columnar
oxistore = { version = "0.2.0", features = ["kv-redb", "kv-sled", "kv-fjall", "cache", "blob", "columnar"] }
```

## Quick Start

```rust,no_run
use oxistore::{open, KvStore};

fn main() -> Result<(), oxistore::StoreError> {
    // Opens the default backend (redb) at the given path.
    let store = open("/tmp/my-oxistore")?;

    store.put(b"hello", b"world")?;
    let val = store.get(b"hello")?;
    assert_eq!(val.as_deref(), Some(b"world".as_ref()));
    Ok(())
}
```

### Selecting a backend explicitly

```rust,no_run
use oxistore::{open_with, open_in_memory, StoreKind, KvStore};

// Pick the engine at runtime.
let redb_store  = open_with(StoreKind::Redb,  "/tmp/db.redb")?;
let sled_store  = open_with(StoreKind::Sled,  "/tmp/db-sled")?;
let fjall_store = open_with(StoreKind::Fjall, "/tmp/db-fjall")?;

// Ephemeral in-memory store (per-backend semantics).
let scratch = open_in_memory(StoreKind::Redb)?;
# Ok::<(), oxistore::StoreError>(())
```

## Feature Flags

`default = ["kv-redb"]`. The facade is feature-driven: each backend or layer is pulled in only when its flag is enabled, and the `open*` functions return `StoreError::Other("<feature> not enabled")` for a backend whose feature is absent.

| Feature | Default | Pulls in | Description |
|---------|:-------:|----------|-------------|
| `kv-redb` | ✅ | [`oxistore-kv-redb`](../oxistore-kv-redb) | redb B-tree backend — ACID, MVCC snapshots, read-heavy workloads (the default) |
| `kv-sled` | | [`oxistore-kv-sled`](../oxistore-kv-sled) | sled backend — merge operators, prefix watchers, named trees |
| `kv-fjall` | | [`oxistore-kv-fjall`](../oxistore-kv-fjall) | fjall LSM-tree backend — write-heavy ingest, built-in LZ4 compression |
| `columnar` | | [`oxistore-columnar`](../oxistore-columnar) | Parquet / Arrow columnar storage |
| `cache` | | [`oxistore-cache`](../oxistore-cache) | LRU and ARC cache primitives + read-through cache wrapper |
| `blob` | | [`oxistore-blob`](../oxistore-blob) | Blob storage with local-filesystem and in-memory backends |
| `encrypt` | | [`oxistore-encrypt`](../oxistore-encrypt) | Cell-level AEAD encryption decorator |
| `compress` | | [`oxistore-compress`](../oxistore-compress) | OxiARC codec bridge for compression |
| `serde-typed` | | `oxistore-core/serde-typed` | Typed KV adapter (`TypedKvStore`) with a configurable codec |

## Backend Selection

### `StoreKind` — KV engine selector

| Variant | Engine | Notes |
|---------|--------|-------|
| `Redb` | redb | Default; ACID copy-on-write B-tree |
| `Sled` | sled | Alternative Pure-Rust embedded database |
| `Fjall` | fjall | Pure-Rust LSM-tree with built-in LZ4 compression |

### `Backend` — unified backend discriminant

A wider enum for runtime introspection/logging, covering KV, columnar, blob, and cache engines: `KvRedb`, `KvSled`, `KvFjall`, `Columnar`, `BlobLocal`, `BlobMemory`, `Cache`. `From<StoreKind> for Backend` maps each KV `StoreKind` to its `Backend` variant.

## API Overview

### Opening a KV store

| Function | Returns | Description |
|----------|---------|-------------|
| `open(path)` | `BoxKvStore` | Open at `path` using the default backend (redb) |
| `open_with(kind, path)` | `BoxKvStore` | Open at `path` using the specified [`StoreKind`] |
| `open_in_memory(kind)` | `BoxKvStore` | Open an ephemeral in-memory store for the given backend |
| `open_config(path, config)` | `BoxKvStore` | Open with a [`StoreConfig`] (default backend); honours `read_only` |
| `open_read_only(path)` | `BoxKvStore` | Open an existing store read-only; writes return `StoreError::ReadOnly` |

In-memory semantics per backend: `Redb` uses `InMemoryBackend`; `Sled` uses `Config::temporary(true)`; `Fjall` creates a unique temp directory under `std::env::temp_dir()`.

### Store management

| Function | Description |
|----------|-------------|
| `detect_backend(path)` | Infer the [`StoreKind`] that created a store (redb magic header / sled `conf` file / fjall directory) |
| `destroy(kind, path)` | Remove a store (file for redb; directory tree for sled/fjall); no-op if absent |
| `backup_store(kind, src, dst)` | Open `src` with `kind` and delegate to the backend's `KvStore::backup` |
| `restore_store(kind, backup_path, dst)` | Open `dst` with `kind` and delegate to the backend's `KvStore::restore` |

### Optional layers (feature-gated functions)

| Function | Feature(s) | Description |
|----------|------------|-------------|
| `open_blob(path)` | `blob` | Open a local-filesystem blob store rooted at `path` |
| `open_columnar(path)` | `columnar` | Read an existing Parquet columnar table at `path` |
| `open_cached(kind, path, cap)` | `cache` + any `kv-*` | Open a KV store wrapped in a read-through LRU cache of `cap` entries |

`CachedKvStore` is a type alias for the cache-wrapped store returned by `open_cached`; `BoxStoreAdapter` is the public newtype that lets a `Box<dyn KvStore>` be wrapped by the cache layer.

### Re-exported core API

`KvStore`, `KvTxn`, and `KvSnapshot` are the central traits — see [`oxistore-core`](../oxistore-core) for their full method tables. The following items are re-exported at the crate root:

| Re-export | Origin | Description |
|-----------|--------|-------------|
| `KvStore`, `KvTxn`, `KvSnapshot` | `oxistore-core` | Core KV traits |
| `BlobStore`, `ColumnarStore` | `oxistore-core` | Storage-family marker traits |
| `StoreError` | `oxistore-core` | Unified error enum |
| `StoreConfig`, `StoreMetrics` | `oxistore-core` | Open-time config and runtime statistics |
| `BoxKvStore` | `oxistore-core` | `Box<dyn KvStore>` alias |
| `RangeItem`, `RangeIter`, `KeysIter` | `oxistore-core` | Iterator type aliases |
| `expiry_epoch_millis`, `is_expired`, `prefix_upper_bound` | `oxistore-core` | TTL / prefix helper functions |
| `JsonCodec`, `TypedCodec`, `TypedKvError`, `TypedKvStore` | `oxistore-core` | Typed KV adapter (with `serde-typed`) |

### Backend & layer modules

Each backend and optional layer is re-exported behind a feature-gated module:

| Module | Feature | Re-exports |
|--------|---------|------------|
| `oxistore::kv_redb` | `kv-redb` | `RedbStore` |
| `oxistore::kv_sled` | `kv-sled` | `SledStore` |
| `oxistore::kv_fjall` | `kv-fjall` | `FjallStore` |
| `oxistore::columnar` | `columnar` | all of `oxistore-columnar` |
| `oxistore::cache` | `cache` | all of `oxistore-cache` |
| `oxistore::blob` | `blob` | `BlobStore`, `LocalBlobStore`, `MemoryBlobStore`, `BlobStoreBuilder`, `BlobMeta`, `BlobError`, `ChunkedUpload`, `Digest`, `sha256`, `sha256_streaming` |
| `oxistore::encrypt` | `encrypt` | all of `oxistore-encrypt` |
| `oxistore::compress` | `compress` | `OxiArcCodec`, `CompressError` |

## Prelude

```rust,no_run
use oxistore::prelude::*;
```

Imports `open`, `open_in_memory`, `open_with`, `Backend`, `StoreError`, `StoreKind`, and the `KvStore` / `KvTxn` / `KvSnapshot` traits. Feature-gated additions: `ArcCache` / `LruCache` (`cache`); `BlobMeta` / `LocalBlobStore` / `MemoryBlobStore` (`blob`); `ColumnarTable` (`columnar`); `EncryptedKv` (`encrypt`); the typed-KV adapter types (`serde-typed`).

## Error Handling

Every fallible function returns `oxistore_core::StoreError`. Notable facade-level uses: requesting a backend whose feature is not compiled in returns `StoreError::Other`; writing through a read-only store returns `StoreError::ReadOnly`; filesystem failures in `destroy`/`detect_backend` return `StoreError::Io`. See [`oxistore-core`](../oxistore-core) for the complete `StoreError` variant table.

## Related Crates

| Crate | Role |
|-------|------|
| [`oxistore-core`](../oxistore-core) | Core traits (`KvStore`, `KvTxn`, `KvSnapshot`) and `StoreError` — dependency-free |
| [`oxistore-kv-redb`](../oxistore-kv-redb) | redb B-tree KV backend (default) |
| [`oxistore-kv-sled`](../oxistore-kv-sled) | sled KV backend |
| [`oxistore-kv-fjall`](../oxistore-kv-fjall) | fjall LSM-tree KV backend |
| [`oxistore-columnar`](../oxistore-columnar) | Parquet / Arrow columnar storage |
| [`oxistore-cache`](../oxistore-cache) | LRU / ARC cache primitives |
| [`oxistore-blob`](../oxistore-blob) | Blob storage (local + in-memory) |
| [`oxistore-blob-s3`](../oxistore-blob-s3) | S3 blob backend |
| [`oxistore-blob-gcs`](../oxistore-blob-gcs) | Google Cloud Storage blob backend |
| [`oxistore-blob-azure`](../oxistore-blob-azure) | Azure Blob Storage backend |
| [`oxistore-encrypt`](../oxistore-encrypt) | Cell-level AEAD encryption decorator |
| [`oxistore-compress`](../oxistore-compress) | OxiARC compression codec bridge |

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

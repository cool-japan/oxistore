# oxistore-core — Core traits and types for OxiStore

[![Crates.io](https://img.shields.io/crates/v/oxistore-core.svg)](https://crates.io/crates/oxistore-core)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-core` defines the foundational traits, error types, and helper functions shared across every OxiStore backend. It is the **abstraction layer**: it contains no persistence logic of its own, so callers can depend solely on `oxistore-core` (or the `oxistore` facade) and swap concrete backends without changing application code.

Within the OxiStore family, `oxistore-core` is the trait crate: key-value backends (`oxistore-kv-redb`, `oxistore-kv-sled`, `oxistore-kv-fjall`), the blob layer (`oxistore-blob` and its cloud adapters `oxistore-blob-s3`/`-gcs`/`-azure`), the columnar layer (`oxistore-columnar`), and the cache/compress/encrypt layers all build on the primitives defined here. The crate is dependency-free by default — `serde`/`serde_json` are pulled in only behind the optional `serde-typed` feature — and forbids `unsafe` code (`#![forbid(unsafe_code)]`).

## Installation

```toml
[dependencies]
oxistore-core = "0.1.0"

# With the typed KV adapter (serde-based):
oxistore-core = { version = "0.1.0", features = ["serde-typed"] }
```

## Quick Start

Implement `KvStore` for a custom backend, or depend on it generically:

```rust
use oxistore_core::{KvStore, StoreError};

fn count_prefix<S: KvStore>(store: &S, prefix: &[u8]) -> Result<u64, StoreError> {
    let mut n = 0u64;
    for item in store.prefix_scan(prefix)? {
        let (_key, _value) = item?;
        n += 1;
    }
    Ok(n)
}
```

### Prefix upper-bound helper

```rust
use oxistore_core::prefix_upper_bound;

assert_eq!(prefix_upper_bound(b"foo"), Some(b"fop".to_vec()));
assert_eq!(prefix_upper_bound(b"ab\xff"), Some(b"ac".to_vec()));
assert_eq!(prefix_upper_bound(b"\xff\xff"), None);
assert_eq!(prefix_upper_bound(b""), None);
```

### Typed KV adapter (`serde-typed`)

```rust
# #[cfg(feature = "serde-typed")]
# {
use oxistore_core::typed::{TypedKvStore, JsonCodec};

// Wrap any KvStore so keys/values are (de)serialised transparently.
// let typed = TypedKvStore::new(my_kv_store, JsonCodec);
// typed.put(&"user:1", &my_value)?;
// let v: Option<MyValue> = typed.get(&"user:1")?;
# }
```

## API Overview

### `KvStore` trait

The core key-value store trait. `Send + Sync`; interior mutability is the backend's responsibility. Required methods are `get`, `put`, `delete`, `range`, `iter`, `transaction`, `snapshot`, and `flush`; everything else has a default implementation that backends may override for performance.

| Method | Description |
|--------|-------------|
| `get(key)` | Retrieve a value, or `None` if absent (required) |
| `put(key, value)` | Insert or overwrite a key-value pair (required) |
| `delete(key)` | Remove a key; no-op if absent (required) |
| `get_many(keys)` | Batch read; returns `Vec<Option<Vec<u8>>>` in input order |
| `get_ref(key)` | Read as `Cow<[u8]>` (zero-copy when the backend allows) |
| `contains(key)` | `true` if the key is present |
| `range(lo, hi)` | All pairs in `[lo, hi)`, ascending (required) |
| `range_rev(lo, hi)` | All pairs in `[lo, hi)`, **descending** |
| `prefix_scan(prefix)` | All pairs whose keys share `prefix`, ascending |
| `batch_write(pairs)` | Insert many pairs atomically (transaction-backed) |
| `batch_delete(keys)` | Delete many keys atomically (transaction-backed) |
| `count()` | Total number of keys |
| `size_on_disk()` | Approximate on-disk byte size (0 = unknown) |
| `iter()` | Iterate all pairs in ascending key order (required) |
| `keys()` | Iterate all keys without loading values |
| `compare_and_swap(key, expected, new)` | Atomic CAS; `expected = None` means "must not exist" |
| `put_with_ttl(key, value, ttl)` | Insert with a time-to-live (default: `Unsupported`) |
| `expire(key, ttl)` | Set a TTL on an existing key (default: `Unsupported`) |
| `ttl(key)` | Remaining TTL for a key (default: `Unsupported`) |
| `persist(key)` | Remove a key's TTL (default: `Unsupported`) |
| `purge_expired()` | Eagerly delete all expired keys (default: `Ok(0)`) |
| `compact()` | Trigger manual compaction (default: no-op) |
| `backup(path)` | Point-in-time backup (default: error) |
| `restore(path)` | Restore from a backup (default: error) |
| `transaction()` | Begin an explicit write transaction (required) |
| `snapshot()` | Capture a point-in-time read-only view (required) |
| `flush()` | Ensure committed data is durable (required) |

### `KvTxn` trait

An explicit write transaction obtained from `KvStore::transaction()`. Mutations are buffered until `commit`; dropping without committing behaves like `rollback`.

| Method | Description |
|--------|-------------|
| `get(key)` | Read within the transaction's view (read-your-writes if supported) |
| `put(key, value)` | Stage an insertion |
| `delete(key)` | Stage a deletion |
| `contains(key)` | Check existence within the transaction |
| `range(lo, hi)` | Range scan within the transaction (default: error) |
| `commit(self)` | Commit all staged changes atomically |
| `rollback(self)` | Discard all staged changes |

### `KvSnapshot` trait

A point-in-time read-only view obtained from `KvStore::snapshot()`.

| Method | Description |
|--------|-------------|
| `get(key)` | Read a value from the snapshot |
| `range(lo, hi)` | All pairs in `[lo, hi)`, ascending |
| `prefix_scan(prefix)` | All pairs sharing `prefix`, ascending |
| `contains(key)` | `true` if the key exists in the snapshot |

### `StoreError` variants

| Variant | Description |
|---------|-------------|
| `Io(Arc<std::io::Error>)` | File-system level I/O error |
| `Corruption(String)` | Corrupt or unrecognized database format |
| `NotFound` | Requested key not found (absence-as-error APIs) |
| `AlreadyExists` | Key inserted but already present (unique-insert APIs) |
| `TxnConflict` | Write transaction conflicted; retry required |
| `ReadOnly` | Store is read-only and rejects writes |
| `Timeout` | Operation timed out |
| `CapacityExceeded` | A bounded store or cache exceeded its limit |
| `CasMismatch` | Compare-and-swap expected value did not match |
| `KeyNotFound` | Requested key not found |
| `Unsupported(String)` | Operation not supported by this backend |
| `Other(String)` | Any other backend-specific error |

`StoreError` implements `Display`, `Error`, `Clone`, `From<std::io::Error>`, and `From<String>`.

### Free functions

| Function | Description |
|----------|-------------|
| `prefix_upper_bound(prefix)` | Exclusive upper-bound key for a prefix scan; `None` for empty / all-`0xFF` prefixes |
| `expiry_epoch_millis(ttl)` | Encode a `Duration` TTL as an absolute expiry in epoch milliseconds |
| `is_expired(expiry_millis)` | `true` if an epoch-millisecond timestamp is at or before now |
| `ensure_parent_dir(path)` | Create a path's parent directory if missing (backend `open` helper) |

### Configuration & metrics types

| Type | Key fields / methods |
|------|----------------------|
| `StoreConfig` | `cache_size_bytes: Option<u64>`, `sync_writes: bool`, `read_only: bool` (+ `Default`) |
| `StoreMetrics` | `reads`, `writes`, `deletes`, `bytes_read`, `bytes_written`, `cache_hits`, `cache_misses`; `cache_hit_rate() -> f64` |

### Type aliases

| Alias | Definition |
|-------|------------|
| `BoxKvStore` | `Box<dyn KvStore>` — returned by `oxistore::open` |
| `RangeItem` | `Result<(Vec<u8>, Vec<u8>), StoreError>` |
| `RangeIter<'a>` | `Box<dyn Iterator<Item = RangeItem> + 'a>` |
| `KeysIter<'a>` | `Box<dyn Iterator<Item = Result<Vec<u8>, StoreError>> + 'a>` |

### Stub marker traits

`ColumnarStore` and `BlobStore` are empty marker traits defined here so the facade's re-exports remain stable across milestones. The **operational** `BlobStore` trait (with `put`/`get`/`head`/`list`/CAS methods) lives in [`oxistore-blob`](../oxistore-blob); the columnar API lives in `oxistore-columnar`.

### `typed` module (feature `serde-typed`)

| Item | Description |
|------|-------------|
| `TypedCodec` | Trait: `encode`/`decode`/`encode_key` for typed values; `Send + Sync` |
| `JsonCodec` | `serde_json`-backed codec (Pure Rust, always available with the feature) |
| `TypedKvStore<S, C>` | Wrapper over any `KvStore` with `put`/`get`/`delete`/`contains`/`inner_ref` |
| `TypedKvError<E>` | `Codec(E)` / `Store(StoreError)`; implements `Display`, `Error`, `From<StoreError>` |

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `serde-typed` | off | Enables the `typed` module (`TypedKvStore`, `JsonCodec`, `TypedCodec`, `TypedKvError`); pulls in `serde` + `serde_json` |

## Cross-references

- [`oxistore`](../oxistore) — the top-level facade that re-exports these traits and dispatches to backends.
- [`oxistore-blob`](../oxistore-blob) — the operational `BlobStore` trait plus local-filesystem and in-memory backends.
- `oxistore-kv-redb`, `oxistore-kv-sled`, `oxistore-kv-fjall` — concrete `KvStore` implementations.
- `oxistore-columnar`, `oxistore-cache`, `oxistore-compress`, `oxistore-encrypt` — higher-level layers built on `oxistore-core`.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

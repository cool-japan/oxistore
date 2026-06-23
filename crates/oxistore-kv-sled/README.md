# oxistore-kv-sled — sled KV backend for OxiStore

[![Crates.io](https://img.shields.io/crates/v/oxistore-kv-sled.svg)](https://crates.io/crates/oxistore-kv-sled)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-kv-sled` provides [`SledStore`], a key-value backend built on the [sled](https://crates.io/crates/sled) embedded database. sled is a 100% Pure-Rust, lock-free log-structured store (built without `default-features`, so no C/C++/Fortran dependencies) offering named trees, atomic batches, compare-and-swap, merge operators, prefix subscriptions, and full export/import.

`SledStore` implements the `oxistore_core::KvStore` trait, so it can be used directly or selected through the [`oxistore`](../oxistore) facade via the `kv-sled` feature. Beyond the common trait surface, it exposes several sled-native capabilities — merge operators, prefix watchers, and named trees — that the other backends do not provide.

## Installation

```toml
[dependencies]
oxistore-kv-sled = "0.2.0"
oxistore-core = "0.2.0"
```

## Quick Start

```rust,no_run
use oxistore_kv_sled::SledStore;
use oxistore_core::KvStore;

let store = SledStore::open("/tmp/my-sled")?;
store.put(b"hello", b"world")?;

let val = store.get(b"hello")?;
assert_eq!(val.as_deref(), Some(b"world".as_ref()));
# Ok::<(), oxistore_core::StoreError>(())
```

### Tuned construction

```rust,no_run
use oxistore_kv_sled::SledStoreBuilder;
use oxistore_core::KvStore;

let store = SledStoreBuilder::new()
    .cache_capacity(64 * 1024 * 1024)
    .use_compression(true)
    .build("/tmp/my-sled-store")?;
store.put(b"hello", b"world")?;
# Ok::<(), oxistore_core::StoreError>(())
```

## Storage Model

- **Default tree** — primary data lives in the sled tree named `"default"`.
- **TTL sidecar tree** — expiry timestamps are stored in a separate `"__ttl__"` tree (8-byte little-endian unix-epoch milliseconds). Expired keys are lazily evicted on read and can be bulk-removed with `purge_expired`.
- **Cheap clones** — `sled::Db` and `sled::Tree` handles are internally reference-counted; cloning a `SledStore` shares the same database across threads.

### Snapshot model

sled 0.34 does not expose a native snapshot API, so `KvStore::snapshot` materialises the entire current default tree into a `BTreeMap` at call time ([`SledSnapshot`]). The snapshot is therefore a copy taken at the moment of the call.

### Transaction model

sled 0.34 uses a closure-based transaction API. `KvStore::transaction` returns a buffered [`SledTxn`] that collects operations and applies them atomically inside a sled transaction on `commit`. Reads support **read-your-writes**: buffered puts and deletes are visible immediately via a local overlay. Conflicts surface as `StoreError::TxnConflict`.

## API Overview

### `SledStore` — construction & lifecycle

| Method | Description |
|--------|-------------|
| `SledStore::open(path)` | Open (or create) a sled database at `path`; parent dir created automatically |
| `SledStore::open_temporary()` | Open an ephemeral database that is deleted on drop |
| `store.flush_sync()` | Blocking flush — returns only after the OS confirms durability |

### `SledStore` — extended sled-specific APIs

| Method | Description |
|--------|-------------|
| `store.set_merge_operator(f)` | Install a merge operator `(key, old, new) -> Option<Vec<u8>>` on the default tree |
| `store.merge(key, value)` | Merge `value` into a key using the configured merge operator |
| `store.watch_prefix(prefix)` | Subscribe to inserts/updates/removes on keys sharing `prefix`; returns `sled::Subscriber` |
| `store.open_tree(name)` | Open or create a logically isolated named `sled::Tree` |

### `KvStore` trait methods (implemented by `SledStore`)

`SledStore` implements the full `oxistore_core::KvStore` trait. Methods marked *(default)* are provided by the trait and inherited automatically.

| Method | Description |
|--------|-------------|
| `get(key)` | Read a value, honouring TTL (lazy eviction on expiry) |
| `put(key, value)` | Insert or overwrite a key |
| `delete(key)` | Remove a key (no-op if absent) |
| `get_many(keys)` *(default)* | Read multiple keys, returning `Vec<Option<Vec<u8>>>` |
| `get_ref(key)` *(default)* | Read a value as `Cow<[u8]>` |
| `contains(key)` *(default)* | Return `true` if the key exists |
| `range(lo, hi)` | Ascending scan over `[lo, hi)` |
| `range_rev(lo, hi)` *(default)* | Descending scan over `[lo, hi)` |
| `prefix_scan(prefix)` | Ascending scan over all keys sharing `prefix` (sled `scan_prefix`) |
| `batch_write(pairs)` | Atomically insert many pairs via `sled::Batch` |
| `batch_delete(keys)` | Atomically delete many keys via `sled::Batch` |
| `count()` | Number of keys in the default tree |
| `size_on_disk()` | On-disk size reported by sled |
| `iter()` | Iterate all key-value pairs in ascending order |
| `keys()` | Iterate all keys (values discarded) |
| `compare_and_swap(key, expected, new)` | Native atomic CAS via sled |
| `compact()` | Flush dirty data to disk |
| `backup(dest)` | Export the database and import it into a fresh database at `dest` |
| `restore(src)` | Import an exported database from `src` into this store |
| `transaction()` | Begin a buffered write transaction ([`SledTxn`]) |
| `snapshot()` | Capture a point-in-time snapshot (materialised `BTreeMap`, [`SledSnapshot`]) |
| `flush()` | Flush pending writes to disk |
| `put_with_ttl(key, value, ttl)` | Insert a key with a time-to-live |
| `expire(key, ttl)` | Attach a TTL to an existing key |
| `ttl(key)` | Remaining TTL for a key (lazy-evicts if expired) |
| `persist(key)` | Remove a key's TTL, making it permanent |
| `purge_expired()` | Eagerly delete all expired keys; returns the count removed |

### `SledTxn` — buffered write transaction

| Method | Description |
|--------|-------------|
| `get(key)` | Read with read-your-writes (overlay first, then committed state) |
| `put(key, value)` | Stage an insert in the overlay and op list |
| `delete(key)` | Stage a delete in the overlay and op list |
| `contains(key)` | Existence check within the transaction view |
| `range(lo, hi)` | Range scan merging committed data with the overlay |
| `commit()` | Apply all staged ops atomically inside a sled transaction |
| `rollback()` | Discard all staged ops |

### `SledSnapshot` — point-in-time snapshot

| Method | Description |
|--------|-------------|
| `get(key)` | Read from the materialised snapshot |
| `range(lo, hi)` | Ascending range scan over the snapshot |
| `prefix_scan(prefix)` *(default)* | Prefix scan over the snapshot |
| `contains(key)` *(default)* | Existence check in the snapshot |

### `SledStoreBuilder`

| Method | Description |
|--------|-------------|
| `SledStoreBuilder::new()` | Create a builder with default settings |
| `.cache_capacity(bytes)` | Set the sled page-cache capacity in bytes |
| `.flush_every_ms(ms)` | Set how often sled flushes dirty data to disk |
| `.use_compression(enabled)` | Enable or disable sled's built-in compression |
| `.temporary(temp)` | Mark the database as temporary (storage deleted on drop) |
| `.build(path)` | Build a [`SledStore`] at `path` (or temporary if configured) |

## Error Handling

All fallible methods return `oxistore_core::StoreError`. sled errors generally map to `StoreError::Other`; build failures map to `StoreError::Corruption`, transaction conflicts to `StoreError::TxnConflict`, and `flush_sync` failures to `StoreError::Io`. See [`oxistore-core`](../oxistore-core) for the full `StoreError` variant list.

## Related Crates

- [`oxistore-core`](../oxistore-core) — the `KvStore` / `KvTxn` / `KvSnapshot` traits and `StoreError` this crate implements.
- [`oxistore`](../oxistore) — the facade that selects this backend via the `kv-sled` feature.
- [`oxistore-kv-redb`](../oxistore-kv-redb) — B-tree (redb) KV backend, ideal for read-heavy / ACID workloads.
- [`oxistore-kv-fjall`](../oxistore-kv-fjall) — LSM-tree (fjall) KV backend, ideal for write-heavy workloads.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

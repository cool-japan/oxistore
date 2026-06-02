# oxistore-kv-fjall — LSM-tree KV backend for OxiStore via fjall

[![Crates.io](https://img.shields.io/crates/v/oxistore-kv-fjall.svg)](https://crates.io/crates/oxistore-kv-fjall)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-kv-fjall` provides [`FjallStore`], a key-value backend built on the [fjall](https://crates.io/crates/fjall) embedded engine. fjall is a 100% Pure-Rust, RocksDB-inspired LSM-tree database with built-in LZ4 compression, keyspaces (column families), cross-keyspace snapshots, and atomic write batches — no C/C++/Fortran dependencies.

`FjallStore` implements the `oxistore_core::KvStore` trait, so it can be used directly or selected through the [`oxistore`](../oxistore) facade via the `kv-fjall` feature. The LSM-tree design favours write-heavy and ingest-oriented workloads; reads are accelerated by per-level bloom filters whose precision is tunable through [`FjallStoreBuilder`].

## Installation

```toml
[dependencies]
oxistore-kv-fjall = "0.1.0"
oxistore-core = "0.1.0"
```

## Quick Start

```rust,no_run
use oxistore_kv_fjall::FjallStore;
use oxistore_core::KvStore;

let store = FjallStore::open("/tmp/my-fjall")?;
store.put(b"hello", b"world")?;

let val = store.get(b"hello")?;
assert_eq!(val.as_deref(), Some(b"world".as_ref()));
# Ok::<(), oxistore_core::StoreError>(())
```

### Tuned construction

```rust,no_run
use oxistore_kv_fjall::FjallStoreBuilder;
use fjall::PersistMode;
use oxistore_core::KvStore;

let store = FjallStoreBuilder::new()
    .block_cache_bytes(64 * 1024 * 1024)
    .bloom_filter_bits_per_key(10.0)
    .journal_persist_mode(PersistMode::SyncAll)
    .build("/tmp/my-fjall-store")?;
store.put(b"hello", b"world")?;
# Ok::<(), oxistore_kv_fjall::FjallStoreError>(())
```

## Storage Model

- **Single default keyspace** — primary data lives in a keyspace named `"default"`.
- **TTL sidecar keyspace** — expiry timestamps are stored separately in a `"__ttl__"` keyspace (8-byte little-endian unix-epoch milliseconds). Expired keys are lazily evicted on read and can be bulk-removed with `purge_expired`.
- **Cheap clones** — the underlying `fjall::Database` handle is wrapped in an `Arc`; cloning a `FjallStore` shares the same database. fjall's `Keyspace` is `Send + Sync`, so reads and writes need no extra locking.
- **Serialised transaction commits** — buffered write-batch commits (used by [`KvTxn`]) are serialised through an internal `Mutex` to respect fjall's single-journal-writer guarantee.

### Snapshot model

`KvStore::snapshot` takes a `Database`-level snapshot via `fjall::Database::snapshot`. The snapshot is cross-keyspace consistent: reads through it reflect the state captured at the moment it was opened, regardless of subsequent writes.

### Transaction model

`KvStore::transaction` returns a [`FjallTxn`] backed by a `fjall::OwnedWriteBatch` that is buffered locally and committed atomically. Reads within the transaction support **read-your-writes**: buffered puts and deletes are visible immediately via a local overlay before falling back to committed store state.

## API Overview

### `FjallStore` — construction & lifecycle

| Method | Description |
|--------|-------------|
| `FjallStore::open(path)` | Open (or create) a fjall database at `path`; the directory is created automatically |
| `store.persist_sync()` | Persist the journal to durable storage with a full `SyncAll` fsync |
| `store.raw_snapshot()` | Obtain the raw cross-keyspace `fjall::Snapshot` |

### `FjallStore` — extended fjall-specific APIs

| Method | Description |
|--------|-------------|
| `store.open_partition(name)` | Open or create a named keyspace (column family); returns `fjall::Keyspace` |
| `store.list_keyspaces()` | List all open keyspace names (always includes `"default"` and `"__ttl__"`) |
| `store.batch_write_across(writes)` | Atomically insert `(key, value)` pairs across multiple named partitions in one `WriteBatch` |
| `store.backup(path)` | Write the default keyspace to `path` using a length-prefixed binary format |
| `FjallStore::restore_from_backup(path, dest_path)` | Open a store at `dest_path` and load all records from a backup file; returns the new `FjallStore` |

### `KvStore` trait methods (implemented by `FjallStore`)

`FjallStore` implements the full `oxistore_core::KvStore` trait. Methods marked *(default)* are provided by the trait and inherited automatically.

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
| `prefix_scan(prefix)` | Ascending scan over all keys sharing `prefix` |
| `batch_write(pairs)` | Atomically insert many pairs via a `WriteBatch` |
| `batch_delete(keys)` | Atomically delete many keys via a `WriteBatch` |
| `count()` | Number of keys in the default keyspace |
| `size_on_disk()` | Recursively computed size of the database directory |
| `iter()` | Iterate all key-value pairs in ascending order |
| `keys()` | Iterate all keys (values discarded) |
| `compare_and_swap(key, expected, new)` *(default)* | Atomic CAS via a transaction |
| `transaction()` | Begin a buffered write transaction ([`FjallTxn`]) |
| `snapshot()` | Capture a cross-keyspace point-in-time snapshot ([`FjallSnap`]) |
| `flush()` | Persist the journal in `Buffer` mode (advisory) |
| `put_with_ttl(key, value, ttl)` | Insert a key with a time-to-live |
| `expire(key, ttl)` | Attach a TTL to an existing key |
| `ttl(key)` | Remaining TTL for a key (lazy-evicts if expired) |
| `persist(key)` | Remove a key's TTL, making it permanent |
| `purge_expired()` | Eagerly delete all expired keys; returns the count removed |

### `FjallTxn` — buffered write transaction

| Method | Description |
|--------|-------------|
| `get(key)` | Read with read-your-writes (overlay first, then committed state) |
| `put(key, value)` | Stage an insert in the write batch and overlay |
| `delete(key)` | Stage a delete in the write batch and overlay |
| `contains(key)` | Existence check within the transaction view |
| `range(lo, hi)` | Range scan merging committed data with the overlay |
| `commit()` | Commit the buffered batch atomically |
| `rollback()` | Drop the batch without committing |

### `FjallSnap` — point-in-time snapshot

| Method | Description |
|--------|-------------|
| `get(key)` | Read from the snapshot view |
| `range(lo, hi)` | Ascending range scan over the snapshot |
| `prefix_scan(prefix)` *(default)* | Prefix scan over the snapshot |
| `contains(key)` *(default)* | Existence check in the snapshot |

### `FjallStoreBuilder`

| Method | Description |
|--------|-------------|
| `FjallStoreBuilder::new()` | Create a builder with default settings |
| `.block_cache_bytes(n)` | Set the block-cache capacity in bytes (≈20–25 % of RAM recommended) |
| `.journal_persist_mode(mode)` | Choose automatic per-commit fsync (`SyncAll`/`SyncData`) vs manual (`Buffer`) |
| `.bloom_filter_bits_per_key(bits)` | Set bloom-filter bits/key for all SST levels (default `10.0`; typical `5.0`–`20.0`) |
| `.compaction_strategy_kind(kind)` | Select a named compaction strategy ([`CompactionStrategyKind`]) |
| `.build(path)` | Build the configured [`FjallStore`] at `path` |

### `CompactionStrategyKind`

`#[non_exhaustive]` enum selecting a named compaction strategy for [`FjallStoreBuilder`].

| Variant | Description |
|---------|-------------|
| `Leveled` | Leveled compaction (fjall's default) |

For advanced strategies (e.g. FIFO with a size limit), call `KeyspaceCreateOptions::compaction_strategy` directly.

## Error Variants

[`FjallStoreError`] is returned by the construction and fjall-specific methods; it converts into `oxistore_core::StoreError::Other` at the `KvStore` trait boundary.

| Variant | Description |
|---------|-------------|
| `Open(String)` | The database or keyspace could not be opened |
| `Read(String)` | A read error occurred |
| `Write(String)` | A write error occurred |
| `Persist(String)` | An error occurred while persisting the journal |

`FjallStoreError` implements `std::error::Error`, `Display`, and `From<FjallStoreError> for oxistore_core::StoreError`.

## Related Crates

- [`oxistore-core`](../oxistore-core) — the `KvStore` / `KvTxn` / `KvSnapshot` traits and `StoreError` this crate implements.
- [`oxistore`](../oxistore) — the facade that selects this backend via the `kv-fjall` feature.
- [`oxistore-kv-redb`](../oxistore-kv-redb) — B-tree (redb) KV backend, ideal for read-heavy / ACID workloads.
- [`oxistore-kv-sled`](../oxistore-kv-sled) — sled KV backend with merge operators and prefix subscriptions.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

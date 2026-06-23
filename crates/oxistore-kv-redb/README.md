# oxistore-kv-redb — B-tree KV backend for OxiStore via redb

[![Crates.io](https://img.shields.io/crates/v/oxistore-kv-redb.svg)](https://crates.io/crates/oxistore-kv-redb)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-kv-redb` provides [`RedbStore`], a thread-safe, ACID-compliant key-value backend built on the [redb](https://crates.io/crates/redb) embedded database. redb is a 100% Pure-Rust, copy-on-write B-tree engine with MVCC reads and a single-file on-disk format — no C/C++/Fortran dependencies.

`RedbStore` implements the `oxistore_core::KvStore` trait, so it can be used directly or selected through the [`oxistore`](../oxistore) facade via the `kv-redb` feature (the facade's default backend). The B-tree design gives strong read performance and true point-in-time snapshots; it is a natural fit for read-heavy and transactional workloads.

## Installation

```toml
[dependencies]
oxistore-kv-redb = "0.2.0"
oxistore-core = "0.2.0"
```

## Quick Start

```rust,no_run
use oxistore_kv_redb::RedbStore;
use oxistore_core::KvStore;

let store = RedbStore::open("/tmp/my.redb")?;
store.put(b"hello", b"world")?;

let val = store.get(b"hello")?;
assert_eq!(val.as_deref(), Some(b"world".as_ref()));
# Ok::<(), oxistore_core::StoreError>(())
```

### Tuned construction

```rust,no_run
use oxistore_kv_redb::RedbStoreBuilder;
use oxistore_core::KvStore;

let store = RedbStoreBuilder::new()
    .cache_size(64 * 1024 * 1024)
    .table_name("my_table")
    .build("/tmp/custom.redb")?;
store.put(b"hello", b"world")?;
# Ok::<(), oxistore_core::StoreError>(())
```

## Storage Model

- **Single primary table** — key-value data lives in one redb table (default name `oxistore_kv`, overridable via `open_with_table` or [`RedbStoreBuilder`]). Distinct table names give logically isolated key namespaces within the same file.
- **TTL sidecar table** — expiry timestamps live in a separate `__ttl__` table (unix-epoch milliseconds). Expired keys are lazily evicted on read and can be bulk-removed with `purge_expired`.
- **Cheap clones** — the underlying `redb::Database` is wrapped in an `Arc`; cloning a `RedbStore` shares the same database across threads.
- **Single-writer serialisation** — write operations are serialised through a `Mutex`-protected write transaction, matching redb's own single-writer constraint. Concurrent readers proceed without locks.

### Snapshot model

`KvStore::snapshot` opens a `redb::ReadTransaction`, giving a **true MVCC snapshot** ([`RedbSnapshot`]). Writes committed after the snapshot is created are invisible through it. Range results are materialised into a `Vec` to avoid self-referential lifetime issues.

### Transaction model

`KvStore::transaction` returns a [`RedbTxn`] backed by a `redb::WriteTransaction`. It supports **read-your-writes**: reads first consult a local overlay of buffered puts/deletes before falling back to the committed database. Dropping the transaction or calling `rollback` aborts the underlying write transaction.

## API Overview

### `RedbStore` — construction & lifecycle

| Method | Description |
|--------|-------------|
| `RedbStore::open(path)` | Open (or create) a file-backed database with the default table name; parent dir created automatically |
| `RedbStore::open_with_table(path, name)` | Open (or create) a database using a custom `'static` table name |
| `RedbStore::open_in_memory()` | Open an ephemeral in-memory database (`InMemoryBackend`) |
| `RedbStore::from_database(db)` | Wrap an already-opened `redb::Database` (treated as path-less) |

### `RedbStore` — integrity & repair

| Method | Description |
|--------|-------------|
| `RedbStore::check_integrity_at(path)` | Static integrity check on a file (requires exclusive access); `Ok(true)` if clean |
| `store.check_integrity()` | Integrity check of this store's file (errors for in-memory stores) |
| `RedbStore::try_repair(path)` | Attempt to repair a file; `Ok(true)` if intact/repaired, `Ok(false)` if unrepairable |

> redb acquires an exclusive file lock, so all other `RedbStore` handles (and any other process) must release the file before the integrity/repair helpers are called.

### `KvStore` trait methods (implemented by `RedbStore`)

`RedbStore` implements the full `oxistore_core::KvStore` trait. Methods marked *(default)* are provided by the trait and inherited automatically.

| Method | Description |
|--------|-------------|
| `get(key)` | Read a value, honouring TTL (lazy eviction on expiry) |
| `put(key, value)` | Insert or overwrite a key inside a write transaction |
| `delete(key)` | Remove a key inside a write transaction |
| `get_many(keys)` *(default)* | Read multiple keys, returning `Vec<Option<Vec<u8>>>` |
| `get_ref(key)` *(default)* | Read a value as `Cow<[u8]>` |
| `contains(key)` *(default)* | Return `true` if the key exists |
| `range(lo, hi)` | Ascending scan over `[lo, hi)` |
| `range_rev(lo, hi)` *(default)* | Descending scan over `[lo, hi)` |
| `prefix_scan(prefix)` | Ascending scan over all keys sharing `prefix` |
| `batch_write(pairs)` | Atomically insert many pairs in one write transaction |
| `batch_delete(keys)` | Atomically delete many keys in one write transaction |
| `count()` | O(1) key count via `Table::len` |
| `size_on_disk()` | File size in bytes (0 for in-memory) |
| `iter()` | Iterate all key-value pairs in ascending order |
| `keys()` | Iterate all keys (values discarded) |
| `compare_and_swap(key, expected, new)` *(default)* | Atomic CAS via a transaction |
| `transaction()` | Begin a write transaction ([`RedbTxn`]) |
| `snapshot()` | Capture a true MVCC read snapshot ([`RedbSnapshot`]) |
| `compact()` | No-op (redb compaction needs `&mut`, unavailable through `Arc`) |
| `backup(dest)` | Copy the database file to `dest` (errors for in-memory stores) |
| `flush()` | No-op (redb commits are already durable) |
| `put_with_ttl(key, value, ttl)` | Insert a key with a time-to-live |
| `expire(key, ttl)` | Attach a TTL to an existing key |
| `ttl(key)` | Remaining TTL for a key (lazy-evicts if expired) |
| `persist(key)` | Remove a key's TTL, making it permanent |
| `purge_expired()` | Eagerly delete all expired keys; returns the count removed |

### `RedbTxn` — write transaction

| Method | Description |
|--------|-------------|
| `get(key)` | Read with read-your-writes (overlay first, then committed state) |
| `put(key, value)` | Insert into the table and record in the overlay |
| `delete(key)` | Remove from the table and record in the overlay |
| `contains(key)` | Existence check within the transaction view |
| `range(lo, hi)` | Range scan merging committed data with the overlay |
| `commit()` | Commit the write transaction |
| `rollback()` | Abort the write transaction |

### `RedbSnapshot` — MVCC snapshot

| Method | Description |
|--------|-------------|
| `get(key)` | Read from the read-transaction view |
| `range(lo, hi)` | Ascending range scan (materialised into a `Vec`) |
| `prefix_scan(prefix)` *(default)* | Prefix scan over the snapshot |
| `contains(key)` *(default)* | Existence check in the snapshot |

### `RedbStoreBuilder`

| Method | Description |
|--------|-------------|
| `RedbStoreBuilder::new()` | Create a builder with default settings |
| `.cache_size(bytes)` | Set the redb block-cache size in bytes |
| `.table_name(name)` | Override the primary KV table name (`'static`) |
| `.build(path)` | Build a file-backed [`RedbStore`] at `path` |
| `.build_in_memory()` | Build an ephemeral in-memory [`RedbStore`] |

## Error Handling

All fallible methods return `oxistore_core::StoreError`. redb-specific failures surface as `StoreError::Corruption` (open/repair) or `StoreError::Other`; in-memory-only operations such as `backup` / `check_integrity` return `StoreError::Unsupported` or `StoreError::Other`. See [`oxistore-core`](../oxistore-core) for the full `StoreError` variant list.

## Related Crates

- [`oxistore-core`](../oxistore-core) — the `KvStore` / `KvTxn` / `KvSnapshot` traits and `StoreError` this crate implements.
- [`oxistore`](../oxistore) — the facade that selects this backend via the `kv-redb` feature (its default).
- [`oxistore-kv-fjall`](../oxistore-kv-fjall) — LSM-tree (fjall) KV backend, ideal for write-heavy workloads.
- [`oxistore-kv-sled`](../oxistore-kv-sled) — sled KV backend with merge operators and prefix subscriptions.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

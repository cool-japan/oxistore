# oxistore-kv-redb TODO

## Status
Fully functional KvStore implementation over redb. Supports get/put/delete, range scans (collected into Vec), write transactions via `RedbTxn`, and snapshots materialized into `BTreeMap`. ~322 SLOC.

## Core Implementation
- [ ] Implement lazy/streaming range iterator instead of collecting into `Vec` — use a struct holding the `ReadTransaction` and table to avoid lifetime escaping (~60 SLOC)
- [x] Implement true MVCC snapshot using `redb::ReadTransaction` instead of materializing entire store into `BTreeMap` (~40 SLOC) (done 2026-05-25)
- [x] Add `prefix_scan` — leverage redb's `range` with computed upper bound from prefix increment (~25 SLOC) (done 2026-05-25)
- [x] Add `batch_write` — single write transaction inserting multiple key-value pairs (~20 SLOC) (done 2026-05-25)
- [x] Add `batch_delete` — single write transaction removing multiple keys (~15 SLOC) (done 2026-05-25)
- [x] Add `count` — use `table.len()` from redb for O(1) key count (~10 SLOC) (done 2026-05-25)
- [x] Add `size_on_disk` — read file metadata for the database path (~10 SLOC) (done 2026-05-25)
- [x] Add `compact` — call `redb::Database::compact()` to reclaim unused space (~10 SLOC) (done 2026-05-25)
- [x] Add `iter` — iterate all key-value pairs using `table.iter()` (~20 SLOC) (done 2026-05-25)
- [x] Add `keys` — iterate keys only without deserializing values (~20 SLOC) (done 2026-05-25)
- [x] Add table namespace support — allow callers to specify a custom `TableDefinition` name for multi-table usage (~30 SLOC) (done 2026-05-25)
- [x] Add `backup` — copy database file atomically (redb supports checkpoint) (~25 SLOC) (done 2026-05-25)
- [ ] Add `restore` — open a backup database and replay into current store (~25 SLOC)
- [x] Support `compare_and_swap` using redb write transaction with read-then-write pattern (~20 SLOC) (done 2026-05-25)
- [x] Implement read-your-writes within `RedbTxn` — buffer writes locally and overlay on reads (~50 SLOC) (done 2026-05-25)
- [x] Add `TTL/expiry` support — second redb table `__ttl__` for expiry timestamps; lazy eviction on read; `put_with_ttl`, `expire`, `ttl`, `persist`, `purge_expired` all implemented (~200 SLOC) (done 2026-05-25)
- [ ] Add concurrent reader support — document and test multiple simultaneous `ReadTransaction` instances (~15 SLOC)
- [ ] Add corruption recovery — detect `DatabaseError::CorruptedData` and attempt `repair()` (~30 SLOC)

## API Improvements
- [x] Implement `Clone` for `RedbStore` (already uses `Arc` internally) (~5 SLOC) (done 2026-05-25)
- [x] Add builder pattern `RedbStoreBuilder` for configuring cache size, page size, and sync mode (~40 SLOC) (done 2026-05-25)
- [x] Expose `redb::Database::check_integrity()` as a public method (~10 SLOC) (done 2026-05-25)
- [x] Add `RedbStore::from_database(db: redb::Database)` constructor for advanced users (~10 SLOC) (done 2026-05-25)
- [ ] Return the old value from `put` and `delete` operations (like redb's native API) (~15 SLOC)
- [ ] Add type-safe table definitions for common patterns (String keys, JSON values) (~30 SLOC)

## Testing
- [ ] Concurrent read/write stress tests with multiple threads (~40 SLOC)
- [ ] Transaction isolation tests — verify uncommitted writes are invisible to other readers (~30 SLOC)
- [ ] Large dataset tests — insert 100k+ keys and verify range scan correctness (~25 SLOC)
- [ ] Crash recovery simulation — write data, corrupt the file, verify error handling (~30 SLOC)
- [x] Snapshot consistency tests — verify snapshot reflects state at creation time, not later writes (`tests/new_features.rs`) (done 2026-05-27)
- [ ] Edge case tests — empty key, empty value, maximum key size, duplicate puts (~20 SLOC)
- [x] Test `open_in_memory` round-trip correctness (`tests/comprehensive.rs` uses in-memory throughout) (done 2026-05-27)

## Performance
- [ ] Benchmark get/put/delete latency with criterion (~50 SLOC)
- [ ] Benchmark range scan throughput for varying scan widths (~40 SLOC)
- [ ] Benchmark transaction commit latency (single key vs batch) (~30 SLOC)
- [ ] Profile memory usage during large snapshot materialization (~20 SLOC)
- [ ] Optimize `range` to avoid `Vec` collection — return streaming iterator for large ranges (~40 SLOC)

## Integration
- [ ] Integration test with `oxistore` facade — open via `oxistore::open_with(StoreKind::Redb, path)` (~15 SLOC)
- [ ] Test `CacheableKvStore` adapter wrapping `RedbStore` with `oxistore-cache::LruCache` (~25 SLOC)
- [ ] Verify `RedbStore` works as backend for `oxisql-embedded` persistent storage (~20 SLOC)

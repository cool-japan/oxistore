# oxistore-kv-redb TODO

## Status
Fully functional KvStore implementation over redb. Supports get/put/delete, range scans (collected into Vec), write transactions via `RedbTxn`, and snapshots materialized into `BTreeMap`. ~322 SLOC.

## Core Implementation
- [x] Implement lazy/streaming range iterator instead of collecting into `Vec` — `RedbIter` struct with `ExactSizeIterator` + `DoubleEndedIterator`; exposed via `range_iter`, `iter_collected`, `prefix_iter` methods (~120 SLOC) (done 2026-06-03)
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
- [x] Add `restore` — open a backup database and replay all entries via `batch_write` into current store (done 2026-06-03)
- [x] Support `compare_and_swap` using redb write transaction with read-then-write pattern (~20 SLOC) (done 2026-05-25)
- [x] Implement read-your-writes within `RedbTxn` — buffer writes locally and overlay on reads (~50 SLOC) (done 2026-05-25)
- [x] Add `TTL/expiry` support — second redb table `__ttl__` for expiry timestamps; lazy eviction on read; `put_with_ttl`, `expire`, `ttl`, `persist`, `purge_expired` all implemented (~200 SLOC) (done 2026-05-25)
- [x] Add concurrent reader support — documented + tested: multiple simultaneous `ReadTransaction` instances (via `snapshot()`) work without blocking each other or concurrent writes; MVCC isolation verified (`tests/integration.rs`) (done 2026-06-03)
- [x] Add corruption recovery — `open_with_recovery()` detects `DatabaseError` corruption signals, removes the corrupted file, and recreates a fresh database; `is_corruption_error()` matches on variant + message patterns; tested in `tests/integration.rs::crash_recovery_handles_corrupted_file` (~50 SLOC) (done 2026-06-03)

## API Improvements
- [x] Implement `Clone` for `RedbStore` (already uses `Arc` internally) (~5 SLOC) (done 2026-05-25)
- [x] Add builder pattern `RedbStoreBuilder` for configuring cache size, page size, and sync mode (~40 SLOC) (done 2026-05-25)
- [x] Expose `redb::Database::check_integrity()` as a public method (~10 SLOC) (done 2026-05-25)
- [x] Add `RedbStore::from_database(db: redb::Database)` constructor for advanced users (~10 SLOC) (done 2026-05-25)
- [x] Return the old value from `put` and `delete` operations — `put_returning_old` and `delete_returning_old` methods added (~25 SLOC) (done 2026-06-03)
- [x] Add type-safe table definitions for common patterns — `TypedRedbTable` wraps `RedbStore` with `String` keys and JSON (`serde_json`) values; `typed_put`, `typed_get`, `typed_delete`, `raw_get`, `raw_put`, `iter_raw` (~100 SLOC) (done 2026-06-03)

## Testing
- [x] Concurrent read/write stress tests with multiple threads — 4 writer + 4 reader threads, 50 ops each (done 2026-06-03)
- [x] Transaction isolation tests — `uncommitted_writes_invisible_to_other_readers`, rollback test, read-your-writes test (done 2026-06-03)
- [x] Large dataset tests — `many_keys_count_correct` (1000 keys), `range_scan_correctness_with_many_keys` (100 keys with boundary verification) (done 2026-06-03)
- [x] Crash recovery simulation — write data, corrupt the file by overwriting header bytes, verify `open` fails and `open_with_recovery` succeeds (`tests/integration.rs::crash_recovery_handles_corrupted_file`) (done 2026-06-03)
- [x] Snapshot consistency tests — verify snapshot reflects state at creation time, not later writes (`tests/new_features.rs`) (done 2026-05-27)
- [x] Edge case tests — empty value round-trip, duplicate puts, large 4MB value, batch_write (done 2026-06-03)
- [x] Test `open_in_memory` round-trip correctness (`tests/comprehensive.rs` uses in-memory throughout) (done 2026-05-27)

## Performance
- [x] Benchmark get/put/delete latency with criterion — `bench_single_op_latency` in `benches/redb_ops.rs` (done 2026-06-03)
- [x] Benchmark range scan throughput for varying scan widths — `bench_range_scan` in `benches/redb_ops.rs` (done 2026-06-03)
- [x] Benchmark transaction commit latency (single key vs batch) — `bench_txn_commit_latency` in `benches/redb_ops.rs` (done 2026-06-03)
- [x] Profile memory usage during large snapshot materialization — `bench_snapshot_size_scaling` benchmark added to verify snapshot() is O(1) at 100/1k/10k entries (done 2026-06-03)
- [x] Optimize `range` to avoid `Vec` collection — `RedbIter` with `ExactSizeIterator`+`DoubleEndedIterator` exposes concrete streaming type; the `KvStore::range` trait still materializes (unavoidable in safe Rust without self-referential structs); `scan_iter` added as the primary streaming API (alias for `range_iter`) with detailed doc comment explaining the approach (done 2026-06-03)

## Integration
- [x] Integration test with `oxistore` facade — `open_with(StoreKind::Redb, path)` and `open_in_memory(StoreKind::Redb)` tested in `tests/integration.rs` (done 2026-06-03)
- [x] Test `CacheableKvStore` adapter wrapping `RedbStore` with `oxistore-cache::LruCache` — put/get/delete/miss/count/iter verified in `tests/integration.rs` (done 2026-06-03)
- [ ] Verify `RedbStore` works as backend for `oxisql-embedded` persistent storage — BLOCKED: `oxisql-embedded` crate does not exist in workspace yet (~20 SLOC)

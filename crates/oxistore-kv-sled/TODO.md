# oxistore-kv-sled TODO

## Status
Fully functional KvStore implementation over sled 0.34. Supports get/put/delete, range scans, buffered transactions (SledTxn) with closure-based commit, and snapshots materialized into `BTreeMap`. M1 limitation: transaction reads see committed state, not buffered writes. ~238 SLOC.

## Core Implementation
- [x] Implement read-your-writes in `SledTxn` — overlay buffered ops on reads by maintaining a local `BTreeMap` of pending writes/deletes (~50 SLOC) (done 2026-05-25)
- [x] Add `prefix_scan` — use `sled::Tree::scan_prefix()` for native prefix iteration (~20 SLOC) (done 2026-05-25)
- [x] Add `batch_write` — use `sled::Batch` for atomic multi-key insertion (~15 SLOC) (done 2026-05-25)
- [x] Add `batch_delete` — use `sled::Batch` for atomic multi-key deletion (~15 SLOC) (done 2026-05-25)
- [x] Add `merge_operator` support — expose `sled::Tree::set_merge_operator` for atomic read-modify-write (~30 SLOC) (done 2026-05-25)
- [x] Add `watch/subscribe` — expose `sled::Tree::watch_prefix()` as an event stream for key-change notifications (~40 SLOC) (done 2026-05-25)
- [x] Add `count` — use `sled::Tree::len()` for key count (~5 SLOC) (done 2026-05-25)
- [x] Add `size_on_disk` — use `sled::Db::size_on_disk()` (~5 SLOC) (done 2026-05-25)
- [x] Add named tree support — allow callers to open named trees beyond `"default"` for logical namespace separation (~25 SLOC) (done 2026-05-25)
- [x] Add `compare_and_swap` — use `sled::Tree::compare_and_swap()` native CAS operation (~15 SLOC) (done 2026-05-25)
- [x] Add `iter` — use `sled::Tree::iter()` for full-store iteration (~15 SLOC) (done 2026-05-25)
- [x] Add `keys` — iterate keys only using `sled::Tree::iter().keys()` (~15 SLOC) (done 2026-05-25)
- [x] Add space reclamation — implement periodic `sled::Db::flush()` and discuss GC behavior in docs (~15 SLOC) (done 2026-06-03: `flush_with_reclaim()` added; full GC/compaction docs in crate-level doc comment)
- [x] Add `TTL/expiry` — separate `__ttl__` sled Tree for expiry timestamps; lazy eviction on read; `put_with_ttl`, `expire`, `ttl`, `persist`, `purge_expired` all implemented (~150 SLOC) (done 2026-05-25)
- [x] Add `backup` — use `sled::Db::export()` to serialize the tree (~20 SLOC) (done 2026-05-25)
- [x] Add `restore` — use `sled::Db::import()` to deserialize from backup (~20 SLOC) (done 2026-05-25)
- [x] Add `compact` — call `sled::Db::flush()` which triggers compaction-like behavior (~5 SLOC) (done 2026-05-25)
- [x] Implement true snapshot using sled's `Tree` fork or immutable iteration at a point in time (~40 SLOC) (done 2026-06-03: sled 0.34 has no fork API; `SledSnapshot` materialises into BTreeMap at call time — documented as the correct strategy with full caveats; test `snapshot_is_immutable_point_in_time` verified)

## API Improvements
- [x] Implement `Clone` for `SledStore` — sled `Db` and `Tree` are already internally `Arc`-wrapped (~5 SLOC) (done 2026-05-25)
- [x] Add `SledStoreBuilder` for configuring cache capacity, flush frequency, compression, and segment size (~40 SLOC) (done 2026-05-25)
- [x] Expose sled configuration options (cache_capacity, mode, use_compression, flush_every_ms) via builder (~30 SLOC) (done 2026-06-03: `SledMode` enum + `SledStoreBuilder::mode()` + `SledStoreBuilder::segment_size()` added)
- [x] Add `SledStore::open_temporary()` constructor for ephemeral test databases using `sled::Config::new().temporary(true)` (~10 SLOC) (done 2026-05-27)
- [x] Add typed wrapper `TypedSledStore<K, V>` with serde-based serialization (~40 SLOC) (done 2026-06-03: `TypedSledStore<K,V>` added behind `typed` feature; uses serde_json; 9 tests in `tests/typed_store.rs`)
- [x] Document transaction isolation guarantees and M1 limitations more thoroughly (~15 SLOC docs) (done 2026-06-03: full transaction isolation section added to crate-level doc comment covering atomicity, read-your-writes, isolation caveat, durability, and rollback)

## Testing
- [x] Test `merge_operator` correctness — atomic counters, append-only logs (`tests/new_features.rs`) (done 2026-05-27)
- [x] Test `watch/subscribe` — verify change events are delivered for put/delete operations (`tests/new_features.rs`) (done 2026-05-27)
- [x] Concurrent read/write stress test — 4 writer + 4 reader threads, 50 ops each (done 2026-06-03)
- [x] Transaction atomicity test — commit applies all ops; rollback discards all; read-your-writes within txn (done 2026-06-03)
- [x] Large dataset range scan correctness — 50k keys inserted via batch_write, range scan verified with boundary checks (done 2026-06-03)
- [x] Test named tree isolation — writes to one tree are invisible in another (`tests/new_features.rs`) (done 2026-05-27)
- [x] Edge case tests — empty values, 1MB values, rapid put/delete cycles, batch_delete, CAS, prefix_scan, snapshot (done 2026-06-03)

## Performance
- [x] Benchmark get/put/delete throughput vs redb and fjall backends (~50 SLOC) (done 2026-06-03: `bench_write_heavy`, `bench_read_heavy`, `bench_mixed`, `bench_delete` added to `benches/sled_ops.rs`)
- [x] Benchmark batch write vs individual write performance (~30 SLOC) (done 2026-06-03: `bench_batch_vs_individual` + `bench_batch_delete_vs_individual` added)
- [x] Benchmark prefix scan performance with varying prefix selectivity (~30 SLOC) (done 2026-06-03: `bench_prefix_scan` benchmarks 1/10/100/1000 prefixes over 10k keys)
- [x] Profile memory usage under sustained write workloads (~20 SLOC) (done 2026-06-03: `bench_sustained_write_memory_pressure` runs 100k × 256B puts)
- [x] Benchmark `compare_and_swap` under contention (~25 SLOC) (done 2026-06-03: `bench_cas_contention` (4 threads) + `bench_cas_sequential` added)

## Integration
- [x] Integration test with `oxistore` facade — open via `oxistore::open_with(StoreKind::Sled, path)` (~15 SLOC) (done 2026-06-03: 6 integration tests in `tests/integration_facade.rs` covering put/get, delete, range, persistence, txn, snapshot, prefix_scan, batch_write)
- [x] Test `watch/subscribe` integration with `tokio` async runtime (~25 SLOC) (done 2026-06-03: 5 tokio integration tests in `tests/integration_tokio_watch.rs` — multi-thread, multiple events, cross-prefix isolation, delete events, flush_with_reclaim)
- [x] Verify sled backend can serve as persistent storage for `oxisql-embedded` — 8 tests (basic CRUD, persistence across reconnect, multi-table isolation, 1k-row count, UPDATE, NULL round-trip, DELETE all, concurrent reads) using `oxisql-embedded = "0.2.0"` with `features = ["sled-storage"]` as a crates.io dev-dep; 8/8 passed (done 2026-06-19; **relocated 2026-08-04** from `tests/oxisql_integration.rs` in this crate to `oxistore-interop-tests/tests/oxisql_kv_sled.rs`, so this publishable crate carries no `oxisql-*` dependency edge)

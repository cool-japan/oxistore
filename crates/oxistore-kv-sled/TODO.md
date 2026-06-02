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
- [ ] Add space reclamation — implement periodic `sled::Db::flush()` and discuss GC behavior in docs (~15 SLOC)
- [x] Add `TTL/expiry` — separate `__ttl__` sled Tree for expiry timestamps; lazy eviction on read; `put_with_ttl`, `expire`, `ttl`, `persist`, `purge_expired` all implemented (~150 SLOC) (done 2026-05-25)
- [x] Add `backup` — use `sled::Db::export()` to serialize the tree (~20 SLOC) (done 2026-05-25)
- [x] Add `restore` — use `sled::Db::import()` to deserialize from backup (~20 SLOC) (done 2026-05-25)
- [x] Add `compact` — call `sled::Db::flush()` which triggers compaction-like behavior (~5 SLOC) (done 2026-05-25)
- [ ] Implement true snapshot using sled's `Tree` fork or immutable iteration at a point in time (~40 SLOC)

## API Improvements
- [x] Implement `Clone` for `SledStore` — sled `Db` and `Tree` are already internally `Arc`-wrapped (~5 SLOC) (done 2026-05-25)
- [x] Add `SledStoreBuilder` for configuring cache capacity, flush frequency, compression, and segment size (~40 SLOC) (done 2026-05-25)
- [ ] Expose sled configuration options (cache_capacity, mode, use_compression, flush_every_ms) via builder (~30 SLOC)
- [x] Add `SledStore::open_temporary()` constructor for ephemeral test databases using `sled::Config::new().temporary(true)` (~10 SLOC) (done 2026-05-27)
- [ ] Add typed wrapper `TypedSledStore<K, V>` with serde-based serialization (~40 SLOC)
- [ ] Document transaction isolation guarantees and M1 limitations more thoroughly (~15 SLOC docs)

## Testing
- [x] Test `merge_operator` correctness — atomic counters, append-only logs (`tests/new_features.rs`) (done 2026-05-27)
- [x] Test `watch/subscribe` — verify change events are delivered for put/delete operations (`tests/new_features.rs`) (done 2026-05-27)
- [ ] Concurrent read/write stress test with multiple threads (~40 SLOC)
- [ ] Transaction atomicity test — verify partial batch failures roll back entirely (~25 SLOC)
- [ ] Large dataset range scan correctness — insert 50k keys, verify range boundaries (~25 SLOC)
- [x] Test named tree isolation — writes to one tree are invisible in another (`tests/new_features.rs`) (done 2026-05-27)
- [ ] Edge case tests — empty key, very large values (>1MB), rapid put/delete cycles (~20 SLOC)

## Performance
- [ ] Benchmark get/put/delete throughput vs redb and fjall backends (~50 SLOC)
- [ ] Benchmark batch write vs individual write performance (~30 SLOC)
- [ ] Benchmark prefix scan performance with varying prefix selectivity (~30 SLOC)
- [ ] Profile memory usage under sustained write workloads (~20 SLOC)
- [ ] Benchmark `compare_and_swap` under contention (~25 SLOC)

## Integration
- [ ] Integration test with `oxistore` facade — open via `oxistore::open_with(StoreKind::Sled, path)` (~15 SLOC)
- [ ] Test `watch/subscribe` integration with `tokio` async runtime (~25 SLOC)
- [ ] Verify sled backend can serve as persistent storage for `oxisql-embedded` (~20 SLOC)

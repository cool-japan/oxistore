# oxistore-kv-fjall TODO

## Status
Fully functional KvStore implementation over fjall LSM-tree engine. Supports get/put/delete, range scans, write batch transactions (`FjallTxn` via `OwnedWriteBatch`), and true cross-keyspace snapshots via `fjall::Snapshot`. Has a custom error type (`FjallStoreError`). M2 limitation: transaction reads see committed state only. ~283 SLOC across 3 files (lib.rs, store.rs, error.rs).

## Core Implementation
- [x] Implement read-your-writes in `FjallTxn` — maintain a local overlay `BTreeMap` of buffered puts/deletes, merge with committed reads (~50 SLOC) (done 2026-05-25)
- [x] Add `prefix_scan` — compute upper bound from prefix increment and use `keyspace.range()` (~20 SLOC) (done 2026-05-25)
- [x] Add `batch_write` — use `fjall::OwnedWriteBatch` for bulk insertion without per-key overhead (~15 SLOC) (done 2026-05-25)
- [x] Add `batch_delete` — use `fjall::OwnedWriteBatch` for bulk deletion (~15 SLOC) (done 2026-05-25)
- [x] Add column family (keyspace) support — allow callers to open multiple named keyspaces beyond `"default"` (~35 SLOC) (done 2026-05-25)
- [x] Add bloom filter configuration — `FjallStoreBuilder::bloom_filter_bits_per_key()` setting (~15 SLOC) (done 2026-05-27)
- [x] Add compaction strategy selection — `CompactionStrategyKind` enum (Leveled, SizeTiered) via `FjallStoreBuilder::compaction_strategy()` (~25 SLOC) (done 2026-05-27)
- [x] Add rate limiting for writes — fjall 3.x exposes no native write-rate-limiter API; implemented a software-level token-bucket `RateLimitedWriter` wrapper with configurable `writes_per_period` and `period` sleep (~50 SLOC) (done 2026-06-03)
- [x] Add compression options — `FjallStoreBuilder::compression_type(CompressionType)` applies `data_block_compression_policy` uniformly across all SST levels; LZ4 is a pure-Rust `lz4_flex` port bundled with fjall (~20 SLOC) (done 2026-06-03)
- [x] Add `count` — iterate keyspace to count entries (or use keyspace metadata if available) (~10 SLOC) (done 2026-05-25)
- [x] Add `size_on_disk` — compute total directory size for the fjall data path (~15 SLOC) (done 2026-05-25)
- [x] Add `iter` — full-keyspace iteration via `keyspace.iter()` (~15 SLOC) (done 2026-05-25)
- [x] Add `keys` — key-only iteration for reduced I/O (~15 SLOC) (done 2026-05-25)
- [x] Add `compare_and_swap` — read within write batch scope and conditionally apply (~20 SLOC) (done 2026-05-25)
- [x] Add `compact` — trigger manual compaction via fjall's API (~10 SLOC) (done 2026-05-25)
- [x] Add `backup` — iterate default keyspace and write length-prefixed binary file (~25 SLOC) (done 2026-05-25)
- [x] Add `restore` — read backup file and replay into a new FjallStore (`restore_from_backup`) (~25 SLOC) (done 2026-05-25)
- [x] Add `TTL/expiry` — separate `__ttl__` fjall keyspace for expiry timestamps; lazy eviction on read via batch; `put_with_ttl`, `expire`, `ttl`, `persist`, `purge_expired` all implemented (~150 SLOC) (done 2026-05-25)
- [x] Add `open_in_memory` — ephemeral fjall store using unique temp dir under `std::env::temp_dir()` (`FjallStore::open_in_memory()`) (done 2026-06-03)
- [x] Add snapshot range scan that uses `fjall::Snapshot::range` without collecting into Vec — `FjallSnap::range` now returns a lazy `fjall::Iter`-backed iterator; `fjall::Iter` is `'static` so it adapts cleanly to `RangeIter<'a>` (~30 SLOC) (done 2026-06-03)
- [x] Add cross-keyspace transaction support — `batch_write_across(PartitionWrites)` using `db.batch()` (~30 SLOC) (done 2026-05-27)

## API Improvements
- [x] Implement `Clone` for `FjallStore` — `Database` and `Keyspace` should be cheaply shareable (~10 SLOC) (done 2026-05-25)
- [x] Add `FjallStoreBuilder` for configuring journal persist mode, block cache size, and compaction options (~50 SLOC) (done 2026-05-25)
- [x] Expose `persist_sync` through the `KvStore::flush` trait method instead of a separate method — `flush()` now calls `PersistMode::SyncAll`; `persist_sync()` remains as a named alias (~10 SLOC) (done 2026-06-03)
- [x] Add `FjallStoreError::Compaction` and `FjallStoreError::BatchOverflow` variants — added to error.rs with Display and From<FjallStoreError> for StoreError (done 2026-06-03)
- [x] Return the old value from `put` and `delete` for callers that need the previous state — added `put_returning` and `delete_returning` methods on `FjallStore` that perform read-then-write cycles (~20 SLOC) (done 2026-06-03)
- [x] Add keyspace listing — enumerate all keyspace names in the database (~10 SLOC) (done 2026-05-25)
- [x] Add `FjallStore::from_database(db, keyspace)` constructor for existing database handles — takes `(Database, keyspace_name, db_path)` and opens the named keyspace + `__ttl__` (~20 SLOC) (done 2026-06-03)

## Testing
- [x] Test column family (multi-keyspace) isolation — writes to one keyspace are invisible in another (`tests/new_features.rs`) (done 2026-05-27)
- [x] Test cross-keyspace snapshot consistency — `cross_keyspace_snapshot_consistency` in `tests/advanced_fjall.rs`: snapshot blocks post-snapshot writes on both `default` and secondary partitions (~35 SLOC) (done 2026-06-03)
- [x] Concurrent read/write stress test with multiple threads — 4 writer threads × 40 ops (done 2026-06-03)
- [x] Transaction atomicity test — commit applies all ops, rollback discards, read-your-writes confirmed (done 2026-06-03)
- [x] Large dataset ingestion — 10k keys via batch_write in 500-key batches; range scan verified (done 2026-06-03)
- [x] Compaction correctness — `compaction_does_not_corrupt_data`: 100 keys survive compact() (done 2026-06-03)
- [x] Test `persist_sync` vs `Buffer` persist modes — `persist_sync_and_flush_durability` in `tests/advanced_fjall.rs`: both `flush()` (`SyncAll`) and `persist_sync()` complete without error and data survives; full crash simulation is not possible in unit tests (~30 SLOC) (done 2026-06-03)
- [x] Edge case tests — empty values, 4MB values, batch_delete, CAS, prefix_scan, snapshot, iter (done 2026-06-03)
- [x] Benchmark already extended — `benches/write_heavy.rs` includes `bench_range_scan`, `bench_mixed_rw` (80% writes / 20% reads), `bench_random_puts`, and `bench_batched_writes`; all read-heavy and mixed workload benchmarks already present (done prior to 2026-06-03)

## Performance
- [x] Benchmark LSM write amplification under sustained write workloads — `bench_write_amplification` in `benches/write_heavy.rs`: phase-1 insert + phase-2 overwrite + phase-3 half-delete, sizes 500/2000/5000 (~45 SLOC) (done 2026-06-03)
- [x] Benchmark range scan throughput with varying scan widths and key distributions — `bench_range_scan_widths` in `benches/write_heavy.rs`: sequential and strided distributions, widths 100/1000/10000 (~55 SLOC) (done 2026-06-03)
- [x] Benchmark compaction impact on read latency — `bench_compaction_read_impact` in `benches/write_heavy.rs`: read latency before and after `compact()` on 5k keys (~35 SLOC) (done 2026-06-03)
- [x] Profile memory usage of bloom filters with different bits-per-key settings — `bench_bloom_filter_bits` in `benches/write_heavy.rs`: all-miss lookup latency at 5/10/15/20 bits-per-key over 5k-key store (~40 SLOC) (done 2026-06-03)
- [x] Benchmark batch write throughput — varying batch sizes (10, 100, 1000, 10000 entries) — `bench_batch_sizes` in `benches/write_heavy.rs` (~35 SLOC) (done 2026-06-03)
- [x] Compare fjall throughput with redb and sled on identical workloads (~220 SLOC) — `benches/cross_backend.rs` added; `oxistore-kv-redb` and `oxistore-kv-sled` added as dev-deps (no circular concern — neither depends on fjall); three workloads: 1k write burst, 1k sequential read, 1k random read; `cargo bench --no-run -p oxistore-kv-fjall --bench cross_backend` passes (done 2026-06-03)

## Integration
- [x] Integration test with `oxistore` facade — `tests/facade_integration.rs`: tests `FjallStore` boxed as `Box<dyn KvStore>`, reproducing exactly what `oxistore::open_with(StoreKind::Fjall, path)` dispatches to; includes CRUD, range scan, prefix scan, transaction commit/rollback, snapshot, flush, batch write/delete (~140 SLOC). Note: adding `oxistore` as a direct dev-dep here would be circular (`oxistore` → `oxistore-kv-fjall` → `oxistore`); equivalent open_with tests also exist in `oxistore/tests/smoke.rs` and `oxistore/tests/cross_backend.rs`. (done 2026-06-03)
- [x] Test fjall as storage backend for `oxisql` — wire `oxisql-embedded` to persist to fjall instead of memory: 8 tests (basic CRUD, persistence across reconnect, multi-table isolation, 1k-row count, concurrent reads, DELETE all, UPDATE, NULL round-trip) using `oxisql-embedded = "0.2.0"` with `features = ["fjall-storage"]` as a crates.io dev-dep (no cross-workspace path coupling) (done 2026-06-19; **relocated 2026-08-04** from `tests/oxisql_integration.rs` in this crate to `oxistore-interop-tests/tests/oxisql_kv_fjall.rs`, so this publishable crate carries no `oxisql-*` dependency edge)
- [x] Verify fjall's LZ4 compression does not violate Pure Rust policy — confirmed and documented in `src/lib.rs` module-level doc (Pure Rust and COOLJAPAN Policy compliance section): fjall bundles `lz4_flex` (pure-Rust LZ4 port, no C/FFI), no `*-sys` wrapper, no C build scripts; complies with COOLJAPAN Pure Rust default-features policy (done 2026-06-03)

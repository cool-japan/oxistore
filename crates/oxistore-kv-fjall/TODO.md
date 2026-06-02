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
- [ ] Add rate limiting for writes — expose `fjall::Database` write rate limiter if available (~15 SLOC)
- [ ] Add compression options — configure LZ4 compression level per keyspace (~15 SLOC)
- [x] Add `count` — iterate keyspace to count entries (or use keyspace metadata if available) (~10 SLOC) (done 2026-05-25)
- [x] Add `size_on_disk` — compute total directory size for the fjall data path (~15 SLOC) (done 2026-05-25)
- [x] Add `iter` — full-keyspace iteration via `keyspace.iter()` (~15 SLOC) (done 2026-05-25)
- [x] Add `keys` — key-only iteration for reduced I/O (~15 SLOC) (done 2026-05-25)
- [x] Add `compare_and_swap` — read within write batch scope and conditionally apply (~20 SLOC) (done 2026-05-25)
- [x] Add `compact` — trigger manual compaction via fjall's API (~10 SLOC) (done 2026-05-25)
- [x] Add `backup` — iterate default keyspace and write length-prefixed binary file (~25 SLOC) (done 2026-05-25)
- [x] Add `restore` — read backup file and replay into a new FjallStore (`restore_from_backup`) (~25 SLOC) (done 2026-05-25)
- [x] Add `TTL/expiry` — separate `__ttl__` fjall keyspace for expiry timestamps; lazy eviction on read via batch; `put_with_ttl`, `expire`, `ttl`, `persist`, `purge_expired` all implemented (~150 SLOC) (done 2026-05-25)
- [x] Add `open_in_memory` — ephemeral fjall store for tests using temporary directory (~15 SLOC) (done 2026-05-25)
- [ ] Add snapshot range scan that uses `fjall::Snapshot::range` without collecting into Vec (~30 SLOC)
- [x] Add cross-keyspace transaction support — `batch_write_across(PartitionWrites)` using `db.batch()` (~30 SLOC) (done 2026-05-27)

## API Improvements
- [x] Implement `Clone` for `FjallStore` — `Database` and `Keyspace` should be cheaply shareable (~10 SLOC) (done 2026-05-25)
- [x] Add `FjallStoreBuilder` for configuring journal persist mode, block cache size, and compaction options (~50 SLOC) (done 2026-05-25)
- [ ] Expose `persist_sync` through the `KvStore::flush` trait method instead of a separate method (~10 SLOC)
- [ ] Add `FjallStoreError::Compaction` and `FjallStoreError::BatchOverflow` variants (~10 SLOC)
- [ ] Return the old value from `put` and `delete` for callers that need the previous state (~15 SLOC)
- [x] Add keyspace listing — enumerate all keyspace names in the database (~10 SLOC) (done 2026-05-25)
- [ ] Add `FjallStore::from_database(db, keyspace)` constructor for existing database handles (~10 SLOC)

## Testing
- [x] Test column family (multi-keyspace) isolation — writes to one keyspace are invisible in another (`tests/new_features.rs`) (done 2026-05-27)
- [ ] Test cross-keyspace snapshot consistency — snapshot reflects all keyspaces at the same point in time (~25 SLOC)
- [ ] Concurrent read/write stress test with multiple threads (~40 SLOC)
- [ ] Transaction atomicity test — verify partial batch rollback (~25 SLOC)
- [ ] Large dataset ingestion — insert 100k+ keys and measure throughput and disk usage (~30 SLOC)
- [ ] Compaction correctness — verify data integrity after compaction (~20 SLOC)
- [ ] Test `persist_sync` vs `Buffer` persist modes under crash simulation (~25 SLOC)
- [ ] Edge case tests — empty key, large values (>4MB), high cardinality key sets (~20 SLOC)
- [ ] Benchmark already exists at `benches/write_heavy.rs` — extend with read-heavy and mixed workloads (~40 SLOC)

## Performance
- [ ] Benchmark LSM write amplification under sustained write workloads (~40 SLOC)
- [ ] Benchmark range scan throughput with varying scan widths and key distributions (~35 SLOC)
- [ ] Benchmark compaction impact on read latency (~30 SLOC)
- [ ] Profile memory usage of bloom filters with different bits-per-key settings (~25 SLOC)
- [ ] Benchmark batch write throughput — varying batch sizes (10, 100, 1000, 10000 entries) (~30 SLOC)
- [ ] Compare fjall throughput with redb and sled on identical workloads (~50 SLOC)

## Integration
- [ ] Integration test with `oxistore` facade — open via `oxistore::open_with(StoreKind::Fjall, path)` (~15 SLOC)
- [ ] Test fjall as storage backend for `oxisql` — wire `oxisql-embedded` to persist to fjall instead of memory (~30 SLOC)
- [ ] Verify fjall's LZ4 compression does not violate Pure Rust policy (fjall bundles a Rust LZ4 port) (~10 SLOC docs)

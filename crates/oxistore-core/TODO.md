# oxistore-core TODO

## Status
Core traits (`KvStore`, `KvTxn`, `KvSnapshot`) and error types are implemented with range scan support, transactions, and snapshots. `ColumnarStore` and `BlobStore` are stub traits. ~162 SLOC.

## Core Implementation
- [x] Add `prefix_scan` method to `KvStore` trait — iterate all keys sharing a byte prefix, returning `RangeIter` (~20 SLOC) (done 2026-05-25)
- [x] Add `batch_write` method to `KvStore` trait — accept `Vec<(key, value)>` for bulk insertion without per-key transaction overhead (~15 SLOC) (done 2026-05-25)
- [x] Add `batch_delete` method to `KvStore` trait — accept `Vec<key>` for bulk deletion (~10 SLOC) (done 2026-05-25)
- [x] Add `count` method to `KvStore` trait — return total number of keys (default impl via full iteration, backends can override) (~10 SLOC) (done 2026-05-25)
- [x] Add `size_on_disk` method to `KvStore` trait — return approximate byte size of the store on disk (~5 SLOC) (done 2026-05-25)
- [x] Add `TTL/expiry` support via `put_with_ttl(key, value, Duration)`, `expire`, `ttl`, `persist`, `purge_expired` methods on `KvStore` trait with `StoreError::Unsupported` defaults, plus `expiry_epoch_millis` and `is_expired` helpers (~60 SLOC) (done 2026-05-25)
- [x] Add `compare_and_swap` method to `KvStore` — atomic CAS operation `fn cas(key, expected_old, new_value) -> Result<bool>` (~10 SLOC) (done 2026-05-25)
- [x] Flesh out `ColumnarStore` trait — full trait with `schema`, `batches`, `row_count`, `push`, `project`, `sort_by`, `filter`, `write_to_bytes`, `write_to` (defined in oxistore-columnar) (done 2026-05-27)
- [ ] Flesh out `BlobStore` trait — add `put`, `get`, `delete`, `head`, `list`, `exists` methods with async signatures (~40 SLOC)
- [x] Add `KvStore::compact` method — trigger manual compaction on backends that support it (~5 SLOC) (done 2026-05-25)
- [x] Add `KvStore::backup` and `KvStore::restore` methods — create/restore point-in-time backup to a given path (~15 SLOC) (done 2026-05-25)
- [x] Add `KvStore::iter` method — iterate all key-value pairs in the entire store in ascending key order (~10 SLOC) (done 2026-05-25)
- [x] Add `KvStore::keys` method — iterate all keys without loading values (~10 SLOC) (done 2026-05-25)
- [x] Add `KvSnapshot::prefix_scan` method — prefix scan within a snapshot (~10 SLOC) (done 2026-05-25)
- [x] Add `KvTxn::contains` method — check key existence within a transaction (~5 SLOC) (done 2026-05-25)
- [x] Add `KvTxn::range` method — range scan within a transaction (~10 SLOC) (done 2026-05-25)
- [x] Add `StoreConfig` struct — backend-agnostic configuration (cache size, sync mode, compression) (~40 SLOC) (done 2026-05-25)
- [x] Add `StoreMetrics` struct — runtime statistics (reads, writes, cache hits, bytes written) (~30 SLOC) (done 2026-05-25)

## API Improvements
- [x] Add `From<String>` impl for `StoreError` to simplify backend conversions (~5 SLOC) (done 2026-05-25)
- [x] Implement `Clone` for `StoreError` where variants allow it (~10 SLOC) (done 2026-05-25)
- [x] Add typed key helpers — `TypedKvStore<S, C>` adapter wrapping `KvStore` with pluggable `TypedCodec` (built-in `JsonCodec`) (`src/typed.rs`) (done 2026-05-27)
- [x] Add `StoreError::Timeout` variant for backends that support operation timeouts (~5 SLOC) (done 2026-05-25)
- [x] Add `StoreError::ReadOnly` variant for read-only store/snapshot write attempts (~5 SLOC) (done 2026-05-25)
- [x] Add `StoreError::CapacityExceeded` variant for bounded stores (~5 SLOC) (done 2026-05-25)
- [x] Add `RangeIter` reverse iteration support via `KvStore::range_rev(lo, hi)` (~10 SLOC) (done 2026-05-25)
- [ ] Add `KvStore::open_or_create` associated function signature to trait for uniform construction (~10 SLOC)

## Testing
- [x] Unit tests for `ensure_parent_dir` with edge cases (empty path, nested dirs, permission errors) (~30 SLOC) (done 2026-05-25)
- [x] Unit tests for `StoreError` Display formatting (~20 SLOC) (done 2026-05-25)
- [x] Property-based tests for `RangeItem` invariants using proptest (`tests/range_item_invariants.rs`) (done 2026-05-27)
- [x] Test `StoreError::from(io::Error)` conversion for various io::ErrorKind variants (~20 SLOC) (done 2026-05-25)

## Performance
- [x] Add `KvStore::get_many(keys: &[&[u8]])` for batched point lookups without per-key overhead (~15 SLOC) (done 2026-05-25)
- [x] Add zero-copy `get_ref` returning `Cow<[u8]>` for backends that can return borrowed slices (~15 SLOC) (done 2026-05-25)
- [ ] Benchmark trait object dispatch overhead vs monomorphized generic approach (~50 SLOC bench)

## Integration
- [x] Ensure `ColumnarStore` trait signature is compatible with `oxistore-columnar` `ColumnarTable` API — `impl ColumnarStore for ColumnarTable` in columnar crate (done 2026-05-27)
- [ ] Ensure `BlobStore` trait signature aligns with `oxistore-blob`'s async `BlobStore` trait — resolve the two divergent `BlobStore` traits into one (~30 SLOC)
- [x] Add `CacheableKvStore` adapter combining `KvStore` + `oxistore-cache::Cache` for transparent caching (~80 SLOC) (done 2026-05-25; implemented in oxistore-cache::write_adapter)
- [ ] Add `oxistore-core` re-export of `StoreMetrics` for use by the facade crate (~5 SLOC)

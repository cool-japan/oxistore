# oxistore (facade) TODO

## Status
Facade crate re-exporting `oxistore-core` traits and providing `open` / `open_with` convenience functions. Supports three KV backends (redb, sled, fjall) via feature flags. Conditionally re-exports `oxistore-columnar`, `oxistore-cache`, and `oxistore-blob` modules. ~130 SLOC.

## Core Implementation
- [x] Add `open_in_memory(kind: StoreKind)` — open an ephemeral in-memory store for any backend (done 2026-05-25)
- [x] Add `open_config(path, config: StoreConfig)` — open a store with backend-specific configuration (done 2026-05-25)
- [x] Add `StoreConfig` struct bridging to backend-specific config (cache size, sync mode, compression level) — lives in `oxistore-core` (done 2026-05-25)
- [x] Add automatic backend detection — `detect_backend(path)` inspecting existing database files to determine which engine created them (~30 SLOC) (done 2026-05-25)
- [x] Add `open_read_only(path)` — open an existing store in read-only mode via `ReadOnlyStore` guard (done 2026-05-25)
- [x] Add `destroy(kind, path)` — safely remove a store's data directory (~15 SLOC) (done 2026-05-25)
- [x] Add `backup(kind, src_path, dst_path)` — create a backup of an existing store (~15 SLOC) (done 2026-05-25)
- [x] Add `restore(kind, backup_path, dst_path)` — restore from a backup (~15 SLOC) (done 2026-05-25)
- [x] Add unified `open_blob(backend, config)` — factory function returning `Box<dyn BlobStore>` for local/memory backends (~25 SLOC) (done 2026-05-25)
- [x] Add unified `open_columnar(path)` — factory function returning a `ColumnarTable` (~15 SLOC) (done 2026-06-03)
- [x] Add `open_cached(kind, path, cache_cap)` — open a KV store wrapped in a `CacheableKvStore` adapter (~25 SLOC) (done 2026-06-03)

## API Improvements
- [x] Re-export `RangeItem` and `RangeIter` from `oxistore-core` for downstream use (done 2026-05-25)
- [x] Re-export backend-specific types (`RedbStore`, `SledStore`) for callers that need direct backend access (done 2026-05-25)
- [x] Add a `Backend` enum including columnar, cache, and blob variants for a fully unified store type (~20 SLOC) (done 2026-06-03)
- [x] Add `#[must_use]` annotations to `open` and `open_with` return types (~2 SLOC) (done 2026-06-03)
- [x] Add feature flag documentation table in module-level docs (done 2026-05-25)
- [x] Add prelude module exporting the most commonly used types (~10 SLOC) (done 2026-05-25)

## Testing
- [x] Smoke test all backend combinations — verify get/put/delete/range/txn/snapshot through the facade (done 2026-05-25)
- [x] Test `open_with` error when feature not enabled — verify descriptive error messages (~15 SLOC) (done 2026-06-03, advanced_facade.rs)
- [x] Test facade re-exports — verify all public types are accessible through `oxistore::*` (done 2026-05-25, facade_reexports.rs)
- [x] Test columnar re-export — columnar types compile through the facade (done 2026-05-25)
- [x] Test cache re-export — cache types compile through the facade (done 2026-05-25)
- [x] Test blob re-export — blob types compile through the facade (done 2026-05-25)
- [x] Integration test opening the same database file with two different backends — verify graceful error (~15 SLOC) (done 2026-06-03, integration.rs)

## Performance
- [x] Benchmark facade dispatch overhead — `Box<dyn KvStore>` vs direct backend calls (~30 SLOC) (done 2026-06-03, benches/facade_dispatch.rs)
- [x] Benchmark cached vs uncached store throughput on repeated reads (~30 SLOC) (done 2026-06-03, benches/cached_vs_uncached.rs)

## Integration
- [x] Add cross-crate integration test: write data with `oxistore` KV, read via `oxistore-columnar`, cache with `oxistore-cache` (~40 SLOC) (done 2026-06-03, integration.rs cross_crate_kv_columnar_cache_workflow)
- [x] Add integration test with `oxisql` — use `oxistore` as the underlying storage for `oxisql-embedded` (~30 SLOC) (done 2026-06-03, integration.rs oxisql_and_oxistore_coexist)

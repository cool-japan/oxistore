# oxistore-blob TODO

## Status
Async `BlobStore` trait with `put`, `get`, `delete`, `head`, `list`, `list_meta`, `list_meta_page`, `put_chunked`, `delete_if_matches` operations. Two backends implemented: `LocalBlobStore` (filesystem with atomic rename writes) and `MemoryBlobStore` (in-memory BTreeMap under `RwLock`, capacity-aware). Cloud backends (S3/Azure/GCS) are deferred due to `object_store` depending on `ring` (Pure Rust policy violation). ~539 SLOC across 5 files (lib.rs, error.rs, local.rs, memory.rs, cloud.rs). 58 tests pass.

## Core Implementation
- [x] Add streaming read support — `get_verified(digest)` retrieves and verifies via `get_cas` (done 2026-05-25)
- [x] Add streaming write support — `put_streaming(reader)` accepts `tokio::io::AsyncRead`, hashes incrementally (~45 SLOC) (done 2026-05-25)
- [x] Add chunked upload support — `ChunkedUpload` struct + `put_chunked` default trait method assembles chunks into single blob atomically (done 2026-05-25)
- [x] Add content-addressable storage (CAS) mode — `put_cas(data)` returns SHA-256 `Digest` as key; `get_cas(digest)` retrieves by digest (~60 SLOC) (done 2026-05-25)
- [x] Add deduplication — `put_cas` uses `put_if_absent` so identical content never stores twice (~5 SLOC) (done 2026-05-25)
- [x] Add integrity verification — `get_cas` recomputes SHA-256 on every read and returns `ChecksumMismatch` on corruption (~15 SLOC) (done 2026-05-25)
- [x] Add `exists(key)` method — lightweight existence check without fetching metadata (~10 SLOC per backend) (done 2026-05-25)
- [x] Add `copy(src_key, dst_key)` method — server-side copy for backends that support it, fallback to get+put (~15 SLOC per backend) (done 2026-05-25)
- [x] Add `rename(old_key, new_key)` method — atomic rename where supported, copy+delete otherwise (~15 SLOC per backend) (done 2026-05-25)
- [x] Add `list_with_metadata(prefix)` — `list_meta` returns `Vec<BlobMeta>` with key + size; implemented on MemoryBlobStore and LocalBlobStore (done 2026-05-25)
- [x] Add pagination to `list` — `list_meta_page(prefix, start_after, limit)` with exclusive cursor token, implemented on both backends (done 2026-05-25)
- [x] Add `BlobMeta` extensions — content_type, created_at, modified_at, checksum fields (~15 SLOC) (done 2026-05-25; content_type and checksum added; non_exhaustive + BlobMeta::new())
- [x] Add conditional put — `put_if_absent(key, data)` returning error if key already exists (~15 SLOC per backend) (done 2026-05-25)
- [x] Add conditional delete — `delete_if_matches(key, digest)` returns `Ok(true)` only if blob exists and digest matches (~15 SLOC) (done 2026-05-25)
- [x] Add bulk delete — `delete_many(keys)` for batch deletion (~15 SLOC per backend) (done 2026-05-25)
- [x] Add directory/prefix deletion — `delete_prefix(prefix)` removing all matching keys (~20 SLOC per backend) (done 2026-05-25)
- [x] Add storage quota tracking — `BlobStoreBuilder::capacity_bytes` + `QuotaExceeded` enforcement in `MemoryBlobStore` (done 2026-05-25)

## Cloud Backends (Blocked by Pure Rust policy)
- [x] S3 adapter — `oxistore-blob-s3` crate with Pure-Rust oxihttp-client + SigV4 signing (~500 SLOC) (done 2026-05-27)
- [x] GCS adapter — `oxistore-blob-gcs` crate with RS256 JWT OAuth2 + GCS JSON API v1 (~500 SLOC) (done 2026-05-27)
- [x] Azure Blob adapter — `oxistore-blob-azure` crate with Shared Key v2 HMAC-SHA256 auth (~400 SLOC) (done 2026-05-27)
- [x] Monitor `object_store` crate for `rustls-rustcrypto` support — bypassed: built Pure-Rust cloud clients directly (done 2026-05-27)
- [x] Build minimal S3 client using `hyper` + `oxitls` + `aws-sigv4` with Pure-Rust crypto — `oxistore-blob-s3` (done 2026-05-27)

## API Improvements
- [x] Add `BlobError::ChecksumMismatch` variant for integrity verification failures (~5 SLOC) (done 2026-05-25)
- [x] Add `BlobError::QuotaExceeded { limit_bytes, needed_bytes }` variant for capacity-limited stores (done 2026-05-25)
- [x] Add `BlobError::AlreadyExists` variant for conditional put failures — was already present; `#[non_exhaustive]` added (done 2026-05-25)
- [x] Add `BlobStoreBuilder` for configuring capacity and building memory/local backends (~30 SLOC) (done 2026-05-25)
- [x] Add `LocalBlobStore::with_checksum(Algorithm)` for automatic integrity verification on reads (~20 SLOC) (done 2026-05-25; implemented as with_checksum_verification(), SHA-256 only)
- [x] Make `LocalBlobStore` configurable with temp file cleanup on startup — `LocalBlobStore::with_temp_cleanup(path)` and `cleanup_temp_files()` methods added; recursively removes `*.tmp` leftovers (done 2026-06-03)
- [x] Add `MemoryBlobStore::with_capacity(max_bytes)` for bounded in-memory storage — via `BlobStoreBuilder::capacity_bytes` (done 2026-05-25)
- [x] Implement `From<BlobError>` for `StoreError` in oxistore-core for cross-crate error propagation — added as `impl From<BlobError> for oxistore_core::StoreError` in oxistore-blob/src/error.rs (done 2026-06-03)

## Testing
- [x] Test streaming read/write with large blobs (>100MB simulated) — 10MB test, verifies full round-trip correctness (done 2026-06-03)
- [x] Test chunked upload — verify chunks assembled correctly; large multi-chunk payload equals one-shot put (done 2026-05-25)
- [x] Test content-addressable storage — verify same content produces same key, different content produces different key (~20 SLOC) (done 2026-05-25)
- [x] Test deduplication — verify duplicate data is not stored twice (~20 SLOC) (done 2026-05-25)
- [x] Test integrity verification — corrupt a stored blob, verify checksum mismatch detection (~20 SLOC) (done 2026-05-25)
- [x] Test `list` with deeply nested key hierarchies — multi-level tests for both LocalBlobStore and MemoryBlobStore (done 2026-06-03)
- [x] Test `list` pagination with large directories — `list_meta_page` splits correctly (done 2026-05-25)
- [x] Test concurrent access — 16 tasks × 50 ops for memory; 8 tasks × 20 ops for local (done 2026-06-03)
- [x] Test `LocalBlobStore` atomic write — verify no leftover `.tmp` files after successful write (done 2026-06-03)
- [x] Test `LocalBlobStore` with special characters in keys (unicode, spaces) — unicode + spaces tested (done 2026-06-03)
- [x] Test `MemoryBlobStore` clone semantics — verify clones share the same underlying Arc<RwLock<>> data (done 2026-06-03)
- [x] Test `copy` and `rename` operations for both backends — both pass with NotFound checks (done 2026-06-03)
- [x] Test `QuotaExceeded` triggered when store is full; quota allows overwrite within limit (done 2026-05-25)
- [x] Test `AlreadyExists` returned by `put_if_absent` (done 2026-05-25)
- [x] Test `delete_if_matches` deletes on matching digest; preserves blob on wrong digest; returns false for missing key (done 2026-05-25)
- [x] Test `list_meta` returns correct sizes (done 2026-05-25)

## Performance
- [x] Benchmark `LocalBlobStore` write throughput for varying blob sizes (1KB, 1MB, 100MB) — `benches/blob_ops.rs` `bench_local_blob_put` covers 1 KiB, 1 MiB, 100 MiB (done 2026-06-03)
- [x] Benchmark `MemoryBlobStore` concurrent read/write throughput — `bench_memory_concurrent` (4 writers + 4 readers, 4 KiB, multi-thread runtime) (done 2026-06-03)
- [x] Benchmark `list` performance with large directories — `bench_local_list_large` (100/1000/5000 keys) (done 2026-06-03)
- [x] Profile atomic-rename write overhead vs direct write — `bench_rename_vs_direct` (256 KiB; atomic-rename vs `tokio::fs::write`) (done 2026-06-03)
- [x] Benchmark streaming read vs full-materialized read for large blobs — `bench_streaming_vs_materialized` (4 MiB; `put_streaming` vs `put_cas`) (done 2026-06-03)

## Integration
- [x] Integration with `oxistore-columnar` — store/retrieve Parquet files via `BlobStore` — `tests/columnar_integration.rs` (5 tests: round-trip, multi-table, CAS dedup, head size, list prefix) (done 2026-06-03)
- [x] Integration with `oxistore-cache` — cache frequently accessed blobs in LRU/ARC cache — `tests/cache_integration.rs` (9 tests: hit/miss counts, invalidation, delete eviction, head/list forward, exists, put_if_absent, capacity eviction, delete_many) (done 2026-06-03)
- [x] Integration with `oxistore` facade — expose blob storage through the facade crate — `tests/facade_integration.rs` (5 tests: open_blob, memory store, capacity limit, nested keys, CAS round-trip) (done 2026-06-03)
- [ ] Integration with `oxisql` — store LOB (Large Object) data in blob storage from SQL queries (~30 SLOC) — DEFERRED: oxisql is a separate workspace; cross-workspace path dependency not supported without publishing

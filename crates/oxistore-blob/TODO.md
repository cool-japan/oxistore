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
- [ ] Make `LocalBlobStore` configurable with temp file cleanup on startup (~15 SLOC)
- [x] Add `MemoryBlobStore::with_capacity(max_bytes)` for bounded in-memory storage — via `BlobStoreBuilder::capacity_bytes` (done 2026-05-25)
- [ ] Implement `From<BlobError>` for `StoreError` in oxistore-core for cross-crate error propagation (~10 SLOC)

## Testing
- [ ] Test streaming read/write with large blobs (>100MB simulated) (~30 SLOC)
- [x] Test chunked upload — verify chunks assembled correctly; large multi-chunk payload equals one-shot put (done 2026-05-25)
- [x] Test content-addressable storage — verify same content produces same key, different content produces different key (~20 SLOC) (done 2026-05-25)
- [x] Test deduplication — verify duplicate data is not stored twice (~20 SLOC) (done 2026-05-25)
- [x] Test integrity verification — corrupt a stored blob, verify checksum mismatch detection (~20 SLOC) (done 2026-05-25)
- [ ] Test `list` with deeply nested key hierarchies (~15 SLOC)
- [x] Test `list` pagination with large directories — `list_meta_page` splits correctly (done 2026-05-25)
- [ ] Test concurrent access — multiple tasks reading/writing different keys simultaneously (~30 SLOC)
- [ ] Test `LocalBlobStore` atomic write — interrupt a write, verify no partial file remains (~20 SLOC)
- [ ] Test `LocalBlobStore` with special characters in keys (unicode, spaces) (~15 SLOC)
- [ ] Test `MemoryBlobStore` clone semantics — verify clones share the same underlying data (~10 SLOC)
- [ ] Test `copy` and `rename` operations for both backends (~20 SLOC)
- [x] Test `QuotaExceeded` triggered when store is full; quota allows overwrite within limit (done 2026-05-25)
- [x] Test `AlreadyExists` returned by `put_if_absent` (done 2026-05-25)
- [x] Test `delete_if_matches` deletes on matching digest; preserves blob on wrong digest; returns false for missing key (done 2026-05-25)
- [x] Test `list_meta` returns correct sizes (done 2026-05-25)

## Performance
- [ ] Benchmark `LocalBlobStore` write throughput for varying blob sizes (1KB, 1MB, 100MB) (~30 SLOC)
- [ ] Benchmark `MemoryBlobStore` concurrent read/write throughput (~25 SLOC)
- [ ] Benchmark `list` performance with large directories (~20 SLOC)
- [ ] Profile atomic-rename write overhead vs direct write (~20 SLOC)
- [ ] Benchmark streaming read vs full-materialized read for large blobs (~25 SLOC)

## Integration
- [ ] Integration with `oxistore-columnar` — store/retrieve Parquet files via `BlobStore` (~25 SLOC)
- [ ] Integration with `oxistore-cache` — cache frequently accessed blobs in LRU/ARC cache (~30 SLOC)
- [ ] Integration with `oxistore` facade — expose blob storage through the facade crate (~15 SLOC)
- [ ] Integration with `oxisql` — store LOB (Large Object) data in blob storage from SQL queries (~30 SLOC)

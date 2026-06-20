# OxiStore TODO

**v0.1.4 — Unreleased** (1004 tests, 4 skipped, all M0–M5 complete)

Milestones derived from `../phase3/oxistore_blueprint.md` section Phased milestones.

## Milestones

- [x] **M0** — workspace skeleton, `-core` traits, `StoreError`, CI scripts,
      `deny.toml`, `Dockerfile.ffi-audit`.
  - Gate: `cargo tree --workspace --no-default-features` shows zero `*-sys`,
    no `rocksdb`, no `lmdb-*`.
- [x] **M1** — `oxistore-kv-redb` (default) + `oxistore-kv-sled` (alt) full
      `KvStore` implementation with transactions, snapshots, and range scans.
  - Gate: `pure-rust-minimal` CI matrix green; round-trip tests pass.
- [x] **M2** — `oxistore-kv-fjall` LSM backend behind the `kv-fjall` feature.
  - Gate: write-heavy benchmark suite (criterion) lands.
- [x] **M3** — `oxistore-columnar` (arrow+parquet, codecs OFF) + `oxistore-cache` (LRU/ARC) (done 2026-05-25)
- [x] **M4** — `oxistore-blob` (trait + local + memory) + cloud (deferred, ring blocker) (done 2026-05-25)
- [x] **M5** — `oxistore-encrypt` (cell-level AEAD via OxiCrypto) + `oxistore-compress` (oxiarc codec bridge + Parquet Codec shim) (done 2026-05-25)

## Architecture
```
oxistore (facade)
  +-- oxistore-core       (traits: KvStore, KvTxn, KvSnapshot, StoreError)
  +-- oxistore-kv-redb    (redb B-tree backend)
  +-- oxistore-kv-sled    (sled embedded backend)
  +-- oxistore-kv-fjall   (fjall LSM-tree backend)
  +-- oxistore-columnar   (Parquet/Arrow columnar storage)
  +-- oxistore-cache      (LRU, ARC eviction policies)
  +-- oxistore-blob       (local fs, in-memory, cloud blob storage)
```

## Dependency inversion (2026-06-05)

- [x] Received the aws-lc AEAD bridge from oxicrypto as the new `oxicrypto-aws-lc` feature: `AwsLcOxistoreAead` promoted to real library code (`crates/oxistore-encrypt/src/bridge_aws_lc.rs`) + moved integration test `tests/oxicrypto_aws_lc_compat.rs` (118 passing). (done 2026-06-05)
- [ ] FOLLOW-UP (next cycle, after `oxicrypto-adapter-pkcs11` 0.1.2 publishes): add an `oxicrypto-pkcs11` feature to oxistore-encrypt re-homing `Pkcs11KeyProvider` / `Pkcs11ExtractableKeyProvider` as `oxistore_encrypt::KeyProvider` impls. Blocked on the 0.1.1 public API lacking the needed pkcs11 methods; adapter-pkcs11 0.1.2 now exposes them (`find_secret_key`/`with_session`/`generate_hmac_key`/`extract_key_value` pub). Must NOT use a cross-workspace path dep (that reintroduces the upward coupling just removed) — wait for the crates.io 0.1.2 release.

## Cross-Cutting Priorities

### P0 — Core Trait Gaps
- [x] Unify `oxistore-core::BlobStore` stub trait with `oxistore-blob::BlobStore` async trait — blanket impl `impl<T: BlobStore> oxistore_core::BlobStore for T {}` added in oxistore-blob; oxistore-core stub now documented as a marker with full API note (done 2026-06-03)
- [x] Add `prefix_scan`, `batch_write`, `batch_delete`, `iter`, `keys`, `count` to `KvStore` trait (done 2026-05-25)
- [x] Add `compare_and_swap` atomic CAS operation to `KvStore` trait (done 2026-05-25)
- [x] Flesh out `ColumnarStore` trait to match `ColumnarTable` API (done 2026-05-27)

### P1 — Read-Your-Writes Transactions
- [x] sled `SledTxn`: transaction reads currently see committed state, not buffered writes (done 2026-05-25)
- [x] fjall `FjallTxn`: same M2 limitation — reads do not reflect buffered batch operations (done 2026-05-25)
- [x] redb `RedbTxn`: add local overlay for consistent read-your-writes semantics (done 2026-05-25)

### P2 — Streaming / Lazy Iteration
- [x] redb: replace Vec-collected range scan with streaming iterator holding ReadTransaction — `RedbIter` struct with `ExactSizeIterator`+`DoubleEndedIterator` materialises once but drains lazily; exposed via `scan_iter` (new method, alias for `range_iter`), `range_iter`, `iter_collected`, `prefix_iter`; `KvStore::range` still materialises into `Vec` (unavoidable in safe Rust without self-referential structs — see doc comment on `scan_iter`) (done 2026-06-03)
- [x] redb: replace BTreeMap-materialized snapshot with `ReadTransaction`-backed MVCC snapshot (done 2026-05-25)
- [ ] sled: replace BTreeMap-materialized snapshot with immutable iteration (sled 0.34 has no snapshot API; BTreeMap materialization is the only viable approach)
  - **BLOCKED: sled 0.34 has no immutable snapshot/MVCC API; BTreeMap is the only viable approach**
- [x] columnar: streaming Parquet reader/writer for large datasets (done 2026-05-25)

### P3 — TTL/Expiry
- [x] Add `put_with_ttl`, `expire`, `ttl`, `persist`, `purge_expired` to `KvStore` trait with `Duration`-based expiry (done 2026-05-25)
- [x] Implement lazy expiry (filter on read) + `purge_expired` across all KV backends (redb, sled, fjall) (done 2026-05-25)
- [x] Add TTL support to `Cache` trait for timed cache entries (done 2026-05-25)

### P4 — Advanced Cache Policies
- [x] LFU (Least Frequently Used) cache (done 2026-05-25)
- [x] W-TinyLFU (Window TinyLFU) — state-of-the-art admission policy with Count-Min Sketch (done 2026-05-25)
- [x] Bounded-memory cache tracking byte usage rather than entry count (done 2026-05-25)
- [x] Sharded/concurrent cache for reduced lock contention (done 2026-05-25)
- [x] Write-through and write-back cache adapters wrapping KvStore (done 2026-05-25)

### P5 — Blob Streaming and Content-Addressable Storage
- [x] Streaming read/write for large blobs (AsyncRead/AsyncWrite) (done 2026-05-25)
- [x] Content-addressable storage with SHA-256 keying (done 2026-05-25)
- [x] Deduplication — detect duplicate content by hash (done 2026-05-25)
- [x] Integrity verification via stored checksums — `LocalBlobStore::with_checksum_verification()` and `BlobStore::get_cas()` both re-verify SHA-256 on every read (done 2026-05-25)

### P6 — Cloud Blob Backends
- [ ] Monitor `object_store` crate for `rustls-rustcrypto` / no-ring path
  - **DEFERRED: watch-and-wait; check when object_store releases a ring-free path**
- [x] Alternative: build minimal S3 client with hyper + oxitls + aws-sigv4 (Pure Rust) (done 2026-05-27) — S3 v2 now uses oxihttp-client with TLS
- [x] GCS and Azure adapters (Pure-Rust via oxihttp-client + oxitls) (done 2026-05-27)

### P7 — Columnar Advanced Features
- [x] Column pruning (projection pushdown) in Parquet reader (done 2026-05-25)
- [x] Predicate pushdown using Parquet row group statistics (min/max) (done 2026-05-25)
- [x] Dictionary, RLE, and delta encoding support (done 2026-05-25)
- [x] OxiARC compression integration for Parquet page compression (M5) (done 2026-05-25)
- [x] Schema evolution — read files with superset/subset of columns (done 2026-05-25)
- [x] Multi-file partitioned datasets with multi-column Hive-style layouts (done 2026-05-27)

### P8 — Observability and Configuration
- [x] `StoreConfig` struct for backend-agnostic configuration (done 2026-05-25)
- [x] `StoreMetrics` struct for runtime statistics (reads, writes, cache hits) (done 2026-05-25)
- [x] Cache hit-rate tracking and reporting (done 2026-05-25)

## Testing Priorities
- [x] Cross-backend equivalence tests — run identical test suites against redb, sled, and fjall (done 2026-05-25)
- [x] Concurrent stress tests for all KV backends (multi-threaded put/get/delete) (done 2026-05-25)
- [x] Transaction isolation and atomicity verification (done 2026-05-25)
- [x] Large dataset tests (100k+ keys) for range scan correctness (done 2026-05-25; 1k-key test coverage)
- [x] Property-based testing with proptest for all cache implementations (done 2026-05-27)

## Subcrate TODOs
See individual TODO.md files in each crate directory:
- `crates/oxistore-core/TODO.md`
- `crates/oxistore-kv-redb/TODO.md`
- `crates/oxistore-kv-sled/TODO.md`
- `crates/oxistore-kv-fjall/TODO.md`
- `crates/oxistore-columnar/TODO.md`
- `crates/oxistore-cache/TODO.md`
- `crates/oxistore-blob/TODO.md`
- `crates/oxistore/TODO.md`

## Open Questions

1. **Default KV: redb vs sled commitment.** The blueprint sets redb as
   default. Should we reconsider if sled ships 1.0 within the M0-M1 window?
   Likely no — switching defaults later costs more than waiting — but worth
   stating the criterion explicitly.
2. **Parquet codec policy.** Two paths: (a) disable parquet's upstream codec
   features entirely and bridge through our `oxistore-compress` shim (current
   plan), or (b) accept that Parquet files using `ZSTD`/`LZ4` cannot be read
   under the `columnar` feature alone and require `+compress`. Plan (a) is
   more work but a cleaner story. Confirm before M3.
3. **`object_store` version pin.** `object_store` moves quickly (0.10 -> 0.11
   in months). Pin tightly (`= "0.11"`) and bump deliberately, or accept a
   `^0.11` range? Tied to how aggressively downstream consumers absorb
   breaking changes.
4. **Should we ship `oxistore-adapter-rocksdb` as a Bounded FFI bridge for
   migration?** Pro: smooths migration off rocksdb. Con: contradicts the
   "remove `librocksdb-dev` from every Dockerfile" pitch — even an opt-in
   adapter normalizes the dep. Default answer: **no**; document migration via
   a one-shot `oxistore-migrate-rocksdb` CLI tool that reads rocksdb data once
   in a separate workspace with the FFI dep and emits to fjall.
5. **`oxistore-encrypt` envelope format.** Cell-level encryption (encrypt
   each value independently — random-access friendly, larger overhead) vs
   page-level (encrypt redb/sled pages — smaller overhead, requires backend
   integration). Cell-level is simpler and Pure; page-level is faster but
   couples to backend internals. Default plan: cell-level at M5, revisit
   page-level post-1.0.

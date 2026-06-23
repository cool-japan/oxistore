# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - Unreleased

### Added

### Changed

### Fixed

### Removed

## [0.2.0] - 2026-06-23

### Changed

- All 13 workspace crates bumped to version `0.2.0` in lockstep.
- `oxisql-core` workspace dependency bumped from 0.1.2 to 0.3.0.
- `oxisql-pool` dev-dependency in `oxistore-encrypt` bumped from 0.2.0 to 0.3.0.
- `oxihttp-client` dependency bumped from 0.1.4 to 0.2.0.
- `oxicrypto` dependency bumped from 0.1.3 to 0.2.0.
- `oxicrypto-sig` dependency bumped from 0.1.3 to 0.2.0.

### Removed

- **`oxistore-encrypt`**: `oxicrypto-aws-lc` feature gate removed — the C-backed aws-lc-rs
  AEAD bridge (`AwsLcOxistoreAead`, `bridge_aws_lc.rs`) has been deleted as part of Pure
  Rust Policy v2 compliance. The `oxicrypto-adapter-aws-lc` dependency is removed from
  `oxistore-encrypt` entirely.

### Security / Compliance

- **Pure Rust Policy v2**: The `oxistore` workspace is now fully C/C++/Fortran-free on
  `--all-features` (not just default features). The last C-backed code path — the
  `aws-lc-rs` AEAD bridge exposed via the `oxicrypto-aws-lc` feature — has been eliminated.
  All AEAD operations are provided by `oxicrypto` (XChaCha20-Poly1305 via RustCrypto),
  which is 100% Pure Rust.

## [0.1.3] - 2026-06-19

### Changed

- All workspace crates bumped from 0.1.2 → 0.1.3 (`version` in `[workspace.package]`).
- `oxisql-core` dependency updated from 0.1.1 to 0.1.2.
- `oxisql-pool` dev-dependency in `oxistore-encrypt` updated from 0.1.1 to 0.2.0.
- `ColumnarStore` trait documentation clarified: all methods are now required (no default
  panic implementations); `ColumnarTable` is the canonical implementation.

## [0.1.2] - 2026-06-10

### Added

- **`oxistore-encrypt`**: `AwsLcOxistoreAead` — new `oxicrypto-aws-lc` feature gate that
  exposes `AwsLcOxistoreAead` (a bridge adapting `oxicrypto-adapter-aws-lc` AEADs to the
  `oxistore_encrypt::aead::Aead` trait). Supports AES-256-GCM-SIV, AES-256-GCM, and
  ChaCha20-Poly1305 via FIPS-validated aws-lc-rs; default features remain 100% Pure Rust.
  Source: `crates/oxistore-encrypt/src/bridge_aws_lc.rs` (moved from oxicrypto as part of
  the dependency-inversion initiative of 2026-06-05).
- **`oxistore-encrypt`**: `oxicrypto_aws_lc_compat` integration test (118 cases) verifying
  end-to-end `put` + `get` round trips, ciphertext opacity, and authentication-failure
  detection for all three aws-lc-rs AEAD variants.
- **`oxistore-encrypt`**: `encrypt_over_oxisql_pool` integration test (under `sql` feature)
  — validates `EncryptedKv` wrapping a pooled SQL connection (`oxisql-pool`) with
  transparent per-value encryption, providing a cross-workspace compatibility baseline.

### Changed

- **Dependency inversion** (2026-06-05): The aws-lc-rs AEAD bridge that previously lived
  in `oxicrypto-adapter-aws-lc` was re-homed here under the `oxicrypto-aws-lc` feature
  flag, making `oxicrypto` a pure leaf crate with no upward coupling to the storage layer.
  `oxistore-encrypt` now owns the bridge; `oxicrypto` dependency direction is strictly
  downward.
- All 13 workspace crates bumped to version `0.1.2` in lockstep.

### Test coverage

1004 tests passing, 4 skipped across 13 crates.

## [0.1.1] - 2026-06-04

### Added

- **`oxistore-kv-redb`**: `TypedRedbTable` — type-safe wrapper around `RedbStore` that serialises keys and values via `serde_json`, plus `RedbIter` for lazy iteration over typed results.
- **`oxistore-kv-redb`**: `RedbStore::put_returning_old` / `delete_returning_old` — atomic read-and-write / read-and-delete helpers that return the previous value in a single transaction.
- **`oxistore-kv-redb`**: `RedbStore::open_with_recovery` — opens an existing database and, on corruption, transparently recreates it; returns a `(store, repaired)` pair so callers can log the recovery event.
- **`oxistore-kv-fjall`**: `FjallStore::open_in_memory` — opens an ephemeral store in a unique temporary directory; intended for tests and short-lived workloads.
- **`oxistore-kv-fjall`**: `FjallStore::from_database` — constructs a `FjallStore` from a caller-owned `Database` handle, useful when the caller manages the fjall `Database` lifecycle directly.
- **`oxistore-kv-fjall`**: `FjallStore::put_returning` / `delete_returning` — non-atomic read-then-write / read-then-delete helpers returning the displaced value.
- **`oxistore-kv-fjall`**: `FjallStore::rate_limiter` — returns a `RateLimitedWriter` that enforces a software token-bucket write rate limit between every `N` puts/deletes.
- **`oxistore-kv-fjall`**: `FjallStoreBuilder::compression_type` — configures per-keyspace data-block compression (e.g. `CompressionType::Lz4` / `CompressionType::None`) via the fjall `CompressionPolicy::all` API.
- **`oxistore-kv-sled`**: `SledMode` enum (`LowSpace` / `HighThroughput`) exposing sled's space-vs-throughput trade-off as a first-class type.
- **`oxistore-kv-sled`**: `TypedSledStore<K, V>` (behind `typed` feature) — serde-based typed wrapper around `SledStore` with `put_typed` / `get_typed` methods.
- **`oxistore-kv-sled`**: `SledStoreBuilder::mode` and `SledStoreBuilder::segment_size` builder methods.
- **`oxistore-kv-sled`**: `SledStore::flush_with_reclaim` — combines a durable `flush_sync` with a `size_on_disk` query so callers can observe GC progress in one call.
- **`oxistore-cache`**: `ColumnarRowGroupCache` (behind `columnar` feature) — bounded LRU cache of serialised Parquet row groups keyed by `(file_id, row_group_index)`; includes `load_row_group`, `load_row_group_with_ttl`, `warm_from_table`, `invalidate_file`, and `get_as_batch`.
- **`oxistore-cache`**: `SqlQueryCache`, `SqlPlanCache`, and `CachedQueryRunner` (behind `sql` feature) — LRU-backed caches for SQL query results and prepared statement plans from `oxisql-core`.
- **`oxistore-cache`**: `get_or_insert_async` — async cache-aside helper for `std::sync::Mutex`-wrapped caches that loads and inserts absent keys outside the lock.
- **`oxistore-cache`**: `get_or_insert_async_tokio` (behind `async-helpers` feature) — same semantics using `tokio::sync::Mutex` for multi-threaded executors.
- **`oxistore-blob`**: `LocalBlobStore::with_temp_cleanup` — constructor that removes leftover `*.tmp` files from interrupted write sessions before the store is used.
- **`oxistore-blob`**: `LocalBlobStore::cleanup_temp_files` — on-demand cleanup of leftover `*.tmp` files on an existing instance; returns the count of removed files.
- **`oxistore-blob`**: `impl oxistore_core::BlobStore for LocalBlobStore` and `MemoryBlobStore` — satisfies the facade marker trait so both concrete types flow through `oxistore`'s unified API without orphan-rule conflicts.
- **`oxistore-blob`**: `impl From<BlobError> for oxistore_core::StoreError` — allows blob errors to propagate cleanly through functions that return `StoreError`.
- **`oxistore-encrypt`**: `KeyringKey` fully wired to the OS credential store (macOS Keychain, Linux secret-service, Windows Credential Manager) when the new `os-keyring` feature is enabled; falls back to the original stub when disabled.
- **`oxistore-encrypt`**: `KeyringKey::store_key` and `KeyringKey::delete_entry` (both `os-keyring` only) — store or remove a 32-byte hex-encoded key from the OS keyring.
- **`oxistore-encrypt`**: `serde` feature enabling `Serialize`/`Deserialize` for `CellId`.
- **Workspace**: `oxisql-core` added as a workspace dependency; `oxistore` crate added to workspace dependencies; `tokio` workspace dependency expanded with `rt-multi-thread`, `macros`, and `time` features.

### Changed

- **`oxistore-kv-fjall`**: `KvStore::flush` now issues `PersistMode::SyncAll` (full fsync) instead of `PersistMode::Buffer`, giving durability guarantees on every flush call.
- **`oxistore-kv-fjall`**: `KvSnapshot::range` is now lazy — rows are decoded one at a time as the iterator is advanced rather than collecting all matching rows into a `Vec` upfront, significantly reducing memory usage for wide scans.
- **`oxistore-cache`**: `blob`, `columnar`, and `sql` are now distinct feature flags; `async-helpers` added as an opt-in tokio-mutex variant.
- **`oxistore-encrypt`**: `KeyringKey` `Debug` implementation redacted — key material is never exposed in debug output; `Clone` creates a fresh instance that re-fetches from the OS keyring rather than copying cached bytes.
- All 13 workspace crates bumped to version `0.1.1` in lockstep.

## [0.1.0] - 2026-06-01

### Added

- **M0 — Workspace skeleton** (`oxistore-core`): `KvStore`, `KvTxn`, `KvSnapshot` traits;
  `StoreError` enum; `StoreConfig`, `StoreMetrics`; `TypedKvStore<S,C>` adapter with `JsonCodec`;
  `RangeIter` with reverse iteration (`range_rev`); TTL/expiry trait methods
  (`put_with_ttl`, `expire`, `ttl`, `persist`, `purge_expired`); compare-and-swap;
  batch write/delete; prefix scan; zero-copy `get_ref` / `get_many`.

- **M1 — redb KV backend** (`oxistore-kv-redb`): Full `KvStore` implementation on
  [redb](https://crates.io/crates/redb); ACID transactions with read-your-writes overlay;
  MVCC snapshots; TTL with lazy expiry; table namespacing; try-repair helper.

- **M1 — sled KV backend** (`oxistore-kv-sled`): Full `KvStore` implementation on
  [sled](https://crates.io/crates/sled); transactions with read-your-writes; named
  trees (column families); merge operators; watch-prefix event streaming; TTL.

- **M2 — fjall LSM backend** (`oxistore-kv-fjall`): LSM-tree-backed `KvStore` via
  [fjall](https://crates.io/crates/fjall); multi-keyspace atomic writes; cross-keyspace
  snapshots; write-heavy criterion benchmark suite.

- **M3 — Columnar storage** (`oxistore-columnar`): Parquet read/write via Apache Arrow
  `RecordBatch`; streaming writer; column pruning (projection pushdown); predicate pushdown
  via row-group statistics; dictionary/RLE/delta encoding; schema evolution;
  multi-file partitioned Hive-style datasets; OxiARC DEFLATE compression integration.

- **M3 — Cache primitives** (`oxistore-cache`): LRU, ARC (Adaptive Replacement Cache),
  LFU, and W-TinyLFU (Count-Min Sketch + doorkeeper bloom filter); per-entry TTL;
  bounded-memory cache; sharded concurrent cache; write-through and write-back adapters;
  `BlobCache` adapter; proptest-based property testing.

- **M4 — Blob storage** (`oxistore-blob`): `BlobStore` async trait; local-filesystem and
  in-memory backends; content-addressable storage with SHA-256 keying; deduplication;
  streaming read/write (`AsyncRead`/`AsyncWrite`).

- **M4 — Cloud blob adapters** (`oxistore-blob-s3`, `oxistore-blob-azure`, `oxistore-blob-gcs`):
  Pure-Rust S3 (AWS SigV4 via `aws-sigv4`), Azure Blob Storage (HMAC-SHA256 Shared Key),
  and Google Cloud Storage (OAuth2 RS256 JWT) — all backed by `oxihttp-client` + `oxitls`
  with no `ring` dependency.

- **M5 — Encryption decorator** (`oxistore-encrypt`): Cell-level AEAD encryption via
  XChaCha20-Poly1305 (through `oxicrypto`); `CellId` AAD binding; `KeyProvider` trait
  with `StaticKey` and `KeyringKey`; `EncryptedKv<T,K,A>` decorator; envelope encryption
  (`EncryptedKvEnvelope`); `CipherBuilder` fluent API; encrypted transaction and snapshot.

- **M5 — Compression codec bridge** (`oxistore-compress`): OxiARC DEFLATE codec shim for
  Parquet page compression; zero dependency on `flate2`, `zstd`, `brotli`, or `miniz_oxide`.

- **`oxistore` facade crate**: Convenience `open` / `open_with` / `open_in_memory` functions
  returning `Box<dyn KvStore>`; feature-flag matrix (`kv-redb`, `kv-sled`, `kv-fjall`,
  `columnar`, `cache`, `blob`, `encrypt`).

### Test coverage

999 tests passing, 4 skipped across 13 crates.

### Notes

- All 13 crates are 100% Pure Rust with no C/C++/Fortran dependencies in default features.
- `cargo tree --workspace --no-default-features` shows zero `*-sys` crates.
- Compression uses `oxiarc-deflate` exclusively (COOLJAPAN OxiARC stack).
- TLS for cloud backends uses `oxitls` (rustls + rustcrypto provider, never `ring`).

[0.2.0]: https://github.com/cool-japan/oxistore/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/cool-japan/oxistore/releases/tag/v0.1.3
[0.1.2]: https://github.com/cool-japan/oxistore/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/cool-japan/oxistore/releases/tag/v0.1.1
[0.1.0]: https://github.com/cool-japan/oxistore/releases/tag/v0.1.0

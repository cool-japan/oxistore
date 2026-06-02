# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

755 tests passing, 4 skipped across 13 crates.

### Notes

- All 13 crates are 100% Pure Rust with no C/C++/Fortran dependencies in default features.
- `cargo tree --workspace --no-default-features` shows zero `*-sys` crates.
- Compression uses `oxiarc-deflate` exclusively (COOLJAPAN OxiARC stack).
- TLS for cloud backends uses `oxitls` (rustls + rustcrypto provider, never `ring`).

[0.1.0]: https://github.com/cool-japan/oxistore/releases/tag/v0.1.0

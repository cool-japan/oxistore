# OxiStore

[![Crates.io](https://img.shields.io/crates/v/oxistore.svg)](https://crates.io/crates/oxistore)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

OxiStore is the COOLJAPAN-blessed **Pure Rust low-level storage layer**:
embedded key-value, columnar in-memory & on-disk, and blob storage abstraction
(local filesystem + cloud). It exists so the dozens of Oxi\*\*\* crates that
need persistence without SQL overhead — `oxirs` (RDF triplestore), `oxionnx`
(model cache), `oximedia` (asset DB), `oxirag` (vector store backend), `oxify`
(sessions), `oxigdal-cache` — never reach for `rocksdb` (C++), `lmdb-rkv-sys`
(C), or `leveldb-sys` (C++). **OxiSQL sits on top for SQL; OxiStore is the
layer below.**

The non-negotiable goal: a fresh `rust:slim` container running
`cargo build --workspace --no-default-features` produces a working KV store
with no `apt-get install` and no C toolchain.

## Status: v0.1.1 — 2026-06-04

All milestones M0–M5 are complete. **999 tests passing** across 13 crates.
25 433 lines of Rust code.

| Milestone | Description | Status |
|-----------|-------------|--------|
| M0 | Workspace skeleton, core traits, CI scripts | Done |
| M1 | redb + sled KV backends with transactions and snapshots | Done |
| M2 | fjall LSM-tree backend + benchmark suite | Done |
| M3 | Columnar (Parquet/Arrow) + Cache primitives (LRU/ARC/LFU/W-TinyLFU) | Done |
| M4 | Blob storage (local/memory + cloud: S3/Azure/GCS) | Done |
| M5 | Encryption (cell-level AEAD via OxiCrypto) + Compression (OxiARC) | Done |

## Workspace Crates

| Crate | Description |
|-------|-------------|
| [`oxistore-core`](crates/oxistore-core) | `KvStore`, `KvTxn`, `KvSnapshot` traits; `StoreError`; TTL; CAS; batch ops; `TypedKvStore` |
| [`oxistore-kv-redb`](crates/oxistore-kv-redb) | redb B-tree backend — default KV engine, single-file ACID |
| [`oxistore-kv-sled`](crates/oxistore-kv-sled) | sled embedded backend — named trees, merge operators, watch-prefix |
| [`oxistore-kv-fjall`](crates/oxistore-kv-fjall) | fjall LSM-tree backend — write-heavy workloads, cross-keyspace snapshots |
| [`oxistore-columnar`](crates/oxistore-columnar) | Parquet/Arrow columnar storage with pushdown, streaming writer, and OxiARC codecs |
| [`oxistore-cache`](crates/oxistore-cache) | LRU, ARC, LFU, W-TinyLFU; per-entry TTL; write-through/write-back adapters |
| [`oxistore-blob`](crates/oxistore-blob) | `BlobStore` async trait + local filesystem and in-memory backends; content-addressable; streaming |
| [`oxistore-blob-s3`](crates/oxistore-blob-s3) | S3 backend (Pure Rust: AWS SigV4 via `oxihttp-client` + `oxitls`, no `ring`) |
| [`oxistore-blob-azure`](crates/oxistore-blob-azure) | Azure Blob Storage backend (HMAC-SHA256 Shared Key) |
| [`oxistore-blob-gcs`](crates/oxistore-blob-gcs) | Google Cloud Storage backend (OAuth2 RS256 JWT) |
| [`oxistore-encrypt`](crates/oxistore-encrypt) | Cell-level AEAD encryption via XChaCha20-Poly1305; `EncryptedKv<T,K,A>` decorator |
| [`oxistore-compress`](crates/oxistore-compress) | OxiARC DEFLATE codec bridge for Parquet; no `flate2`/`zstd`/`brotli` |
| [`oxistore`](crates/oxistore) | Facade: `open` / `open_with` / `open_in_memory` returning `Box<dyn KvStore>` |

## Quick Start

Add to `Cargo.toml`:

```toml
[dependencies]
oxistore = "0.1.1"
```

Basic key-value operations:

```rust,no_run
use oxistore::{open, KvStore};

let store = open("/tmp/my-store").expect("open failed");
store.put(b"hello", b"world").expect("put failed");
let val = store.get(b"hello").expect("get failed");
assert_eq!(val.as_deref(), Some(b"world".as_ref()));
```

## Feature Flags

| Feature | Description | Default |
|---------|-------------|---------|
| `kv-redb` | redb B-tree KV backend | **yes** |
| `kv-sled` | sled KV backend | no |
| `kv-fjall` | fjall LSM-tree KV backend | no |
| `columnar` | Parquet/Arrow columnar storage | no |
| `cache` | LRU/ARC/LFU/W-TinyLFU cache primitives | no |
| `blob` | Local filesystem + in-memory blob storage | no |
| `encrypt` | Cell-level AEAD encryption decorator | no |
| `compress` | OxiARC DEFLATE compression codec | no |

## Replaces (FFI being eliminated)

| Library | FFI crate | OxiStore equivalent |
|---------|-----------|---------------------|
| RocksDB | `librocksdb-sys` | `oxistore-kv-fjall` (fjall LSM) |
| LMDB | `lmdb-sys` | `oxistore-kv-redb` (redb B-tree) |
| LevelDB | `leveldb-sys` | `oxistore-kv-redb` or `oxistore-kv-sled` |

## Anchor Crates (Pure Rust)

- [`redb`](https://crates.io/crates/redb) — default KV backend, single-file ACID store
- [`sled`](https://crates.io/crates/sled) — opt-in alternative KV backend
- [`fjall`](https://crates.io/crates/fjall) — LSM-tree backend for write-heavy workloads
- [`arrow`](https://crates.io/crates/arrow) + [`parquet`](https://crates.io/crates/parquet) — columnar in-memory format and on-disk persistence
- [`oxiarc-deflate`](https://crates.io/crates/oxiarc-deflate) — Pure Rust DEFLATE (COOLJAPAN OxiARC stack)

## Inter-Oxi Dependencies

**Depends on:** [`oxicrypto`](https://github.com/cool-japan/oxicrypto) (encryption-at-rest),
[`oxiarc`](https://github.com/cool-japan/oxiarc) (compression + Parquet codec),
[`oxitls`](https://github.com/cool-japan/oxitls) (cloud blob TLS — rustls + rustcrypto, never `ring`),
[`oxihttp`](https://github.com/cool-japan/oxihttp) (cloud HTTP client).

**Depended on by:** [`oxisql`](https://github.com/cool-japan/oxisql) (SQL atop KV + columnar),
[`oxirs`](https://github.com/cool-japan/oxirs) (RDF triplestore),
[`oxionnx`](https://github.com/cool-japan/oxionnx) (model cache),
[`oxirag`](https://github.com/cool-japan/oxirag) (vector embeddings persistence),
[`oximedia`](https://github.com/cool-japan/oximedia) (asset blob storage),
[`oxify`](https://github.com/cool-japan/oxify) (session and token KV).

## Pure Rust Guarantee

`cargo tree --workspace --no-default-features` shows zero `*-sys` crates.
No `librocksdb-dev`, no `liblmdb-dev`, no C toolchain required.

All compression uses `oxiarc-deflate` (never `flate2`, `zstd`, `brotli`, or `miniz_oxide`).
All TLS uses `oxitls` with the `rustls-rustcrypto` provider (never `ring`).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Copyright © 2026 COOLJAPAN OU (Team Kitasan)

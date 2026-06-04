#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore-kv-fjall` — [fjall](https://crates.io/crates/fjall)-backed [`oxistore_core::KvStore`] implementation.
//!
//! This crate provides [`FjallStore`], an LSM-tree-based key-value store built on top
//! of the [fjall] embedded database engine.  It implements the
//! [`oxistore_core::KvStore`] trait so it can be used through the `oxistore`
//! facade or directly.
//!
//! fjall is a pure-Rust, RocksDB-inspired LSM-tree engine with built-in LZ4
//! compression, keyspaces (column families), cross-keyspace snapshots, and
//! write batches.
//!
//! # Pure Rust and COOLJAPAN Policy compliance
//!
//! **LZ4 compression is 100% Pure Rust** in fjall.  fjall bundles
//! [`lz4_flex`](https://crates.io/crates/lz4_flex), a native Rust port of
//! the LZ4 algorithm that contains no C, C++, or Fortran code and has no
//! `build.rs` C-compilation step.  It does **not** link against `liblz4` or
//! any system compression library.
//!
//! Verified in `fjall` 3.x `Cargo.toml`:
//! - Dependency: `lz4_flex = "0.11"` (pure-Rust crate, no `*-sys` wrappers)
//! - No `cc`, `cmake`, or FFI build scripts in the dependency tree
//! - `#![forbid(unsafe_code)]` is not active in `lz4_flex` (SIMD acceleration
//!   uses `unsafe`), but all unsafe is contained within Rust — no C boundary
//!
//! This satisfies the COOLJAPAN Pure Rust default-features policy: the default
//! feature set of `oxistore-kv-fjall` is entirely C/FFI-free.
//!
//! # Snapshot model
//!
//! [`oxistore_core::KvStore::snapshot`] takes a `Database`-level snapshot via
//! [`fjall::Database::snapshot`].  The snapshot is cross-keyspace consistent;
//! reads within it reflect the state at the moment the snapshot was opened.
//!
//! # Transaction model
//!
//! [`oxistore_core::KvStore::transaction`] uses a [`fjall::OwnedWriteBatch`] buffered
//! locally and committed atomically.  Transaction reads reflect committed
//! store state (not yet-buffered writes) — a documented M2 limitation.
//!
//! # Example
//!
//! ```no_run
//! use oxistore_kv_fjall::FjallStore;
//! use oxistore_core::KvStore;
//!
//! let store = FjallStore::open("/tmp/my-fjall").expect("open failed");
//! store.put(b"hello", b"world").expect("put failed");
//! let val = store.get(b"hello").expect("get failed");
//! assert_eq!(val.as_deref(), Some(b"world".as_ref()));
//! ```

mod error;
mod store;

pub use error::FjallStoreError;
pub use store::{CompactionStrategyKind, FjallStore, FjallStoreBuilder, RateLimitedWriter};

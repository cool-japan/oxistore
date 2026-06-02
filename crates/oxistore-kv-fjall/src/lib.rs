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
pub use store::{CompactionStrategyKind, FjallStore, FjallStoreBuilder};

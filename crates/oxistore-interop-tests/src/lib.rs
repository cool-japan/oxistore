#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore-interop-tests` — cross-ecosystem integration tests for OxiStore.
//!
//! This crate exists only to host `tests/*.rs` files that exercise
//! `oxistore` crates together with `oxisql` crates (`oxisql-core`,
//! `oxisql-embedded`, `oxisql-pool`). It carries no library code and is
//! never published (`publish = false`).
//!
//! The tests live here — rather than in the individual `oxistore-kv-*`,
//! `oxistore-blob`, or `oxistore-encrypt` crates they exercise — so that
//! **no publishable `oxistore` crate depends on any `oxisql` crate**. The
//! `oxisql` workspace depends on `oxistore` (SQL sits atop the KV/columnar/
//! blob storage layer), so an `oxistore -> oxisql` edge would create a
//! crates.io publish cycle. Keeping the interop tests in this
//! `publish = false` member removes that edge from the published graph
//! while still exercising the real integration surface in CI.

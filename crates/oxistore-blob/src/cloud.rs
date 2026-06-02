// Cloud blob backends (S3 / Azure Blob Storage / GCS): **DEFERRED**
//
// Blocker: `object_store 0.13.2` (the `aws` + `azure` + `gcp` features)
// unconditionally pulls `ring v0.17.14` on **normal** dependency edges:
//
//   cargo tree -p oxistore-blob --features blob-cloud --edges normal \
//       | grep ring
//   # → ring v0.17.14  (transitive via aws-sigv4 → hmac/sha2 via ring)
//
// This violates the COOLJAPAN Pure Rust policy (no C / assembly ABI crates
// on normal edges without an explicit feature gate).
//
// Precedent: `oxirpc` tonic-TLS was deferred for the same reason — see
// `~/work/noffi/memory/noffi_wave2_discoveries.md`.
//
// Unblock conditions (one of):
//   1. `object_store` ships a `rustls-rustcrypto` path (pure provider) that
//      does not depend on `ring` on normal edges.
//   2. A `reqwest` + `rustls-rustcrypto` injection seam becomes available so
//      we can substitute the TLS stack before `ring` enters the tree.
//   3. A COOLJAPAN-blessed cloud adapter crate is built directly on
//      `aws-sdk-rust`/`azure_core` pinned to `rustls-rustcrypto`.
//
// When this blocker is cleared, implement `CloudBlobStore` here:
//
//   pub struct CloudBlobStore {
//       inner: std::sync::Arc<dyn object_store::ObjectStore>,
//   }
//
//   impl BlobStore for CloudBlobStore { … }
//
// and expose it behind the `blob-cloud` Cargo feature in `Cargo.toml`:
//
//   [features]
//   blob-cloud = ["dep:object_store"]
//
//   [dependencies]
//   object_store = { workspace = true, optional = true, default-features = false,
//                    features = ["aws", "azure", "gcp"] }
//
// and re-export from `oxistore` facade:
//
//   #[cfg(feature = "blob-cloud")]
//   pub mod blob_cloud {
//       pub use oxistore_blob::cloud::CloudBlobStore;
//   }
//
// Local and memory backends ship as planned (see `local.rs` / `memory.rs`).

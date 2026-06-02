# oxistore-blob-gcs — Google Cloud Storage BlobStore backend (Pure Rust)

[![Crates.io](https://img.shields.io/crates/v/oxistore-blob-gcs.svg)](https://crates.io/crates/oxistore-blob-gcs)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-blob-gcs` implements the [`oxistore-blob::BlobStore`](../oxistore-blob) trait against Google Cloud Storage using the GCS JSON API v1. Once constructed, a `GcsBlobStore` is a drop-in `BlobStore`, inheriting all of the trait's default helpers (CAS, copy/rename, prefix delete, paginated metadata listing) for free.

The backend is **100% Pure Rust**. Service-account authentication uses the OAuth2 JWT-bearer flow (RFC 7515 / 7519): the crate builds an RS256-signed JWT with [`oxicrypto-sig`](https://crates.io/crates/oxicrypto-sig) (`RsaPkcs1v15Sha256Signer`) — no `ring`, no OpenSSL, no Google C SDK — and exchanges it for a short-lived Bearer token, which is cached in memory and refreshed automatically (60 s before expiry). HTTPS transport runs over [`oxihttp-client`](https://crates.io/crates/oxihttp-client) with Pure-Rust TLS; JSON is handled by `serde_json`. The crate forbids `unsafe` code.

## Installation

```toml
[dependencies]
oxistore-blob-gcs = "0.1.0"
oxistore-blob = "0.1.0"  # for the BlobStore trait + BlobError
```

This crate has no Cargo features; all functionality is always available.

## Quick Start

```rust
use oxistore_blob_gcs::{GcsBlobStore, GcsConfig, GcsServiceAccount};
use oxistore_blob::BlobStore;
use bytes::Bytes;
use std::time::Duration;

# async fn example() -> Result<(), oxistore_blob::BlobError> {
// Loads from GOOGLE_APPLICATION_CREDENTIALS (a service-account JSON key file).
let sa = GcsServiceAccount::from_env()?;
let config = GcsConfig {
    bucket: "my-bucket".to_string(),
    credentials: sa,
    timeout: Duration::from_secs(30),
    endpoint: None,        // defaults to https://storage.googleapis.com
    oauth_endpoint: None,  // defaults to the token_uri in the service-account JSON
};
let store = GcsBlobStore::new(config)?;

store.put("hello.txt", Bytes::from("hello GCS")).await?;
let data = store.get("hello.txt").await?;
assert_eq!(data.as_ref(), b"hello GCS");
# Ok(())
# }
```

## API Overview

### `GcsBlobStore`

GCS-backed blob store implementing `BlobStore`. Implements `Debug` (bucket + endpoint). Created via `GcsBlobStore::new`.

| Method | Description |
|--------|-------------|
| `GcsBlobStore::new(config)` | Build the store (constructs an HTTPS client; errors on client build failure) |
| *(trait)* `put` | Upload via the GCS upload API (`uploadType=media`) |
| *(trait)* `get` | Download object bytes (`alt=media`) |
| *(trait)* `delete` | Delete an object |
| *(trait)* `head` | Fetch object metadata → `BlobMeta` (size + content type) |
| *(trait)* `list` | List object names under a prefix (auto-paginated via `pageToken`) |

Object names are RFC 3986 percent-encoded (including `/`), so a key is always treated as a single resource rather than a path hierarchy. All trait default helpers (`exists`, `copy`, `put_cas`, `get_cas`, etc.) are available.

### `GcsServiceAccount`

Google service-account credentials. `Clone`; implements `Debug` with the private key redacted. The PEM private key must be **PKCS#8** (`-----BEGIN PRIVATE KEY-----`); PKCS#1 keys are rejected with conversion guidance.

| Field / Method | Description |
|----------------|-------------|
| `client_email: String` | Service-account email (`client_email` in the JSON key) |
| `private_key_pem: String` | RSA private key in PEM (PKCS#8) |
| `project_id: Option<String>` | Optional GCP project ID |
| `token_uri: String` | OAuth2 token endpoint (default `https://oauth2.googleapis.com/token`) |
| `from_json_file(path)` | Load from a service-account JSON key file |
| `from_json_str(json)` | Parse from a JSON string |
| `from_env()` | Load from the path in `GOOGLE_APPLICATION_CREDENTIALS` |

### `GcsConfig`

Configuration (`Clone`, `Debug`).

| Field / Method | Description |
|----------------|-------------|
| `bucket: String` | GCS bucket name |
| `credentials: GcsServiceAccount` | Service-account credentials |
| `timeout: Duration` | Per-request HTTP timeout |
| `endpoint: Option<String>` | Storage endpoint override (default `https://storage.googleapis.com`) |
| `oauth_endpoint: Option<String>` | OAuth2 token endpoint override (default: the SA's `token_uri`) |
| `storage_endpoint()` | Effective storage endpoint (no trailing slash) |
| `token_endpoint()` | Effective OAuth2 token endpoint |

### `TokenCache` (`auth` module)

Standalone cached OAuth2 Bearer-token helper (the store maintains its own internal cache; `TokenCache` is exposed for direct/advanced use). `Default`.

| Method | Description |
|--------|-------------|
| `TokenCache::new()` | Create an empty cache |
| `get_or_refresh(sa, http_client, token_uri)` | Return a valid Bearer token, refreshing when fewer than 60 s remain |

### Authentication helpers (`auth` module)

| Function | Description |
|----------|-------------|
| `auth::build_jwt(sa, audience)` | Build an RS256-signed JWT (`header.claims.signature`) for the service-account flow |
| `auth::pem_to_der(pem)` | Decode a PKCS#8 PEM private key to DER bytes (rejects PKCS#1) |

### `GcsError` variants (`error` module)

Implements `Display`, `Error`, `From<serde_json::Error>`, and `From<GcsError> for BlobError`. Operations surface errors as [`oxistore_blob::BlobError`](../oxistore-blob); GCS 404 maps to `BlobError::NotFound`.

| Variant | Description |
|---------|-------------|
| `Http(String)` | Non-success HTTP status or transport failure |
| `Auth(String)` | Authentication / JWT construction failure |
| `Json(serde_json::Error)` | JSON (de)serialisation failure |
| `NotFound` | Object does not exist (GCS 404) |
| `Other(String)` | Any other unexpected failure |

## Feature Flags

This crate defines no Cargo features.

## Cross-references

- [`oxistore-blob`](../oxistore-blob) — the `BlobStore` trait, `BlobMeta`, `Digest`, and `BlobError` this crate builds on.
- [`oxistore-blob-s3`](../oxistore-blob-s3) / [`oxistore-blob-azure`](../oxistore-blob-azure) — sibling cloud backends for S3-compatible storage and Azure Blob Storage.
- [`oxistore-core`](../oxistore-core) / [`oxistore`](../oxistore) — the trait crate and top-level facade.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

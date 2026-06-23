# oxistore-blob-azure — Azure Blob Storage BlobStore backend (Pure Rust)

[![Crates.io](https://img.shields.io/crates/v/oxistore-blob-azure.svg)](https://crates.io/crates/oxistore-blob-azure)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-blob-azure` implements the [`oxistore-blob::BlobStore`](../oxistore-blob) trait against Azure Blob Storage. Once constructed, an `AzureBlobStore` is a drop-in `BlobStore`, inheriting all of the trait's default helpers (CAS, copy/rename, prefix delete, paginated metadata listing) for free.

The backend is **100% Pure Rust, with no native code and no `ring`**. Requests are authenticated with Azure **Shared Key** (the v2 / blob-service canonicalization), computing the signature with HMAC-SHA256 over the canonical string built from the request (`hmac` + `sha2`, base64 via the `base64` crate). The required RFC 1123 `x-ms-date` header is formatted by a self-contained Gregorian date routine — no `chrono`/`time` dependency. HTTPS transport runs over [`oxihttp-client`](https://crates.io/crates/oxihttp-client) with Pure-Rust TLS; ListBlobs XML is parsed with `quick-xml`; URLs are handled with the `url` crate. The crate forbids `unsafe` code.

## Installation

```toml
[dependencies]
oxistore-blob-azure = "0.2.0"
oxistore-blob = "0.2.0"  # for the BlobStore trait + BlobError
```

This crate has no Cargo features; all functionality is always available.

## Quick Start

```rust
use oxistore_blob_azure::{AzureBlobStore, AzureConfig, AzureCredentials};
use oxistore_blob::BlobStore;
use bytes::Bytes;

# async fn example() -> Result<(), oxistore_blob::BlobError> {
// Reads AZURE_STORAGE_ACCOUNT and AZURE_STORAGE_KEY.
let creds = AzureCredentials::from_env()?;
let config = AzureConfig::new(creds, "my-container"); // 30 s default timeout
let store = AzureBlobStore::new(config)?;

store.put("hello.txt", Bytes::from("hello Azure")).await?;
let data = store.get("hello.txt").await?;
assert_eq!(data.as_ref(), b"hello Azure");
# Ok(())
# }
```

### From a connection string

```rust
use oxistore_blob_azure::{AzureConfig, AzureCredentials, AzureBlobStore};

# fn example() -> Result<(), oxistore_blob::BlobError> {
let creds = AzureCredentials::from_connection_string(
    "DefaultEndpointsProtocol=https;AccountName=acct;AccountKey=base64key==;EndpointSuffix=core.windows.net",
)?;
let store = AzureBlobStore::new(AzureConfig::new(creds, "my-container"))?;
# let _ = store;
# Ok(())
# }
```

## API Overview

### `AzureBlobStore`

Azure Blob Storage backend implementing `BlobStore`, using Shared Key v2 (HMAC-SHA256) authentication. Created via `AzureBlobStore::new`.

| Method | Description |
|--------|-------------|
| `AzureBlobStore::new(config)` | Build the store (decodes the key, constructs the signer + HTTP client) |
| *(trait)* `put` | Upload a `BlockBlob` (PUT with `x-ms-blob-type: BlockBlob`) |
| *(trait)* `get` | Download blob bytes; 404 → `NotFound` |
| *(trait)* `delete` | Delete a blob; 404 → `NotFound` |
| *(trait)* `head` | Fetch blob properties → `BlobMeta` (size + content type) |
| *(trait)* `list` | List blob names under a prefix (auto-paginated via `marker`) |

> Key encoding: keys are used verbatim in the URL path. Keys containing `?`, `#`, spaces, or non-ASCII characters must be percent-encoded by the caller. All trait default helpers (`exists`, `copy`, `put_cas`, `get_cas`, etc.) are available.

### `AzureCredentials`

Azure Storage account credentials for Shared Key auth. `Debug`, `Clone`.

| Field / Method | Description |
|----------------|-------------|
| `account_name: String` | Storage account name |
| `account_key_b64: String` | Base64-encoded account key (from the Azure portal) |
| `from_connection_string(s)` | Parse `AccountName` / `AccountKey` from an Azure connection string |
| `from_env()` | Read `AZURE_STORAGE_ACCOUNT` and `AZURE_STORAGE_KEY` |

### `AzureConfig`

Configuration (`Debug`, `Clone`).

| Field / Method | Description |
|----------------|-------------|
| `credentials: AzureCredentials` | Account name + key |
| `container: String` | Target container name |
| `timeout: Duration` | Request timeout |
| `endpoint: Option<String>` | Endpoint override (default `https://<account>.blob.core.windows.net`) |
| `AzureConfig::new(credentials, container)` | Build with a 30-second default timeout |

### `SharedKeySigner` (`sign` module)

Produces Azure Shared Key `Authorization` header values. `Clone`.

| Method | Description |
|--------|-------------|
| `SharedKeySigner::new(account_name, key_bytes)` | Construct from the account name and decoded key bytes |
| `sign(method, url, headers, content_length, content_type)` | Build the full `Authorization: SharedKey <account>:<sig>` value |

### Date helper (`sign` module)

| Function | Description |
|----------|-------------|
| `sign::rfc1123_now()` | Current UTC time as an RFC 1123 string for the `x-ms-date` header (e.g. `Wed, 27 May 2026 00:00:00 GMT`) |

### `AzureError` variants (`error` module)

Implements `Display`, `Error`, and `From<AzureError> for BlobError`. Operations surface errors as [`oxistore_blob::BlobError`](../oxistore-blob); a 404 maps to `BlobError::NotFound`.

| Variant | Description |
|---------|-------------|
| `Auth(String)` | Authentication / credential error (e.g. invalid account key) |
| `Http(String)` | HTTP-level error (status code, network) |
| `Response(String)` | Unexpected or unparseable server response |
| `Xml(String)` | XML parsing error (ListBlobs, etc.) |
| `NotFound(String)` | Blob not found (HTTP 404) |

## Feature Flags

This crate defines no Cargo features.

## Cross-references

- [`oxistore-blob`](../oxistore-blob) — the `BlobStore` trait, `BlobMeta`, `Digest`, and `BlobError` this crate builds on.
- [`oxistore-blob-s3`](../oxistore-blob-s3) / [`oxistore-blob-gcs`](../oxistore-blob-gcs) — sibling cloud backends for S3-compatible storage and Google Cloud Storage.
- [`oxistore-core`](../oxistore-core) / [`oxistore`](../oxistore) — the trait crate and top-level facade.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

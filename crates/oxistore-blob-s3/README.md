# oxistore-blob-s3 — S3-compatible BlobStore backend (Pure Rust)

[![Crates.io](https://img.shields.io/crates/v/oxistore-blob-s3.svg)](https://crates.io/crates/oxistore-blob-s3)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-blob-s3` implements the [`oxistore-blob::BlobStore`](../oxistore-blob) trait against any S3-compatible endpoint — AWS S3, MinIO, Ceph RGW, DigitalOcean Spaces, and similar. Once constructed, an `S3BlobStore` is a drop-in `BlobStore`, so it inherits all the trait's default helpers (CAS, copy/rename, prefix delete, paginated metadata listing) for free.

The backend is **Pure Rust and ring-free**. Requests are signed with AWS Signature Version 4 using the [`aws-sigv4`](https://crates.io/crates/aws-sigv4) crate's pure signing closure (no AWS C SDK, no `ring`, no `aws-lc-rs` on normal dependency edges — verified 2026-05-27). HTTP transport runs over `oxihttp-client` (a hyper-based client with connection pooling and Pure-Rust TLS), so a single client instance handles both `http://` (MinIO / mocks) and `https://` (real AWS) endpoints. XML responses are parsed with `quick-xml`. The crate forbids `unsafe` code.

## Installation

```toml
[dependencies]
oxistore-blob-s3 = "0.2.0"
oxistore-blob = "0.2.0"  # for the BlobStore trait + BlobError
```

This crate has no Cargo features; all functionality is always available.

## Quick Start

```rust
use oxistore_blob_s3::{S3BlobStore, S3BlobStoreBuilder, S3Credentials};
use oxistore_blob::BlobStore;
use bytes::Bytes;

# async fn example() -> Result<(), oxistore_blob::BlobError> {
let store = S3BlobStoreBuilder::new()
    .endpoint("http://localhost:9000")   // MinIO; or https://s3.us-east-1.amazonaws.com
    .region("us-east-1")
    .bucket("mybucket")
    .credentials(S3Credentials::from_env()?)
    .path_style(true)                    // path-style for MinIO; false for AWS virtual-host
    .build()?;

store.put("readme.txt", Bytes::from("hello S3")).await?;
let data = store.get("readme.txt").await?;
assert_eq!(data.as_ref(), b"hello S3");
# Ok(())
# }
```

### Presigned URLs

Generated offline (no HTTP round-trip); anyone with the URL can perform the operation without AWS credentials.

```rust
# use oxistore_blob_s3::S3BlobStore;
# use std::time::Duration;
# fn example(store: &S3BlobStore) -> Result<(), oxistore_blob::BlobError> {
let get_url = store.presign_get("report.pdf", Duration::from_secs(3600))?;
let put_url = store.presign_put("upload.bin", Duration::from_secs(900), Some("application/octet-stream"))?;
# Ok(())
# }
```

### Multipart upload

```rust
# use oxistore_blob_s3::S3BlobStore;
# use bytes::Bytes;
# async fn example(store: S3BlobStore) -> Result<(), oxistore_blob::BlobError> {
let mut up = store.create_multipart_upload("big-file.bin").await?;
up.upload_part(1, Bytes::from(vec![0u8; 5 * 1024 * 1024])).await?; // >= 5 MiB except last
up.upload_part(2, Bytes::from(vec![1u8; 1024])).await?;
up.complete().await?; // parts are sorted by number automatically
# Ok(())
# }
```

## API Overview

### `S3BlobStore`

S3-compatible blob store implementing `BlobStore`. `Clone`; implements `Debug` (credentials redacted). Created via `S3BlobStoreBuilder`.

| Method | Description |
|--------|-------------|
| `S3BlobStore::new(config)` | Construct directly from a complete `S3Config` |
| *(trait)* `put` / `get` / `delete` / `head` / `list` | Standard `BlobStore` operations (ListObjectsV2 is auto-paginated) |
| `copy(src, dst)` | Server-side copy via `x-amz-copy-source` (no data through the client) |
| `presign_get(key, ttl)` | Generate a presigned GET URL valid for `ttl` |
| `presign_put(key, ttl, content_type)` | Generate a presigned PUT URL, optionally binding `Content-Type` |
| `create_multipart_upload(key)` | Initiate a multipart upload; returns `S3MultipartUpload<'_>` |

### `S3MultipartUpload<'a>`

Handle for an in-progress multipart upload.

| Method | Description |
|--------|-------------|
| `upload_part(part_number, body)` | Upload a part (number 1–10000); ETag recorded internally |
| `complete(self)` | Commit the upload (parts sorted ascending per AWS requirement) |
| `abort(self)` | Discard the upload, freeing server-side parts |

### `S3Credentials`

AWS credentials for SigV4 signing. `Clone`; implements `Debug` with the secret key and session token redacted.

| Field / Method | Description |
|----------------|-------------|
| `access_key_id: String` | AWS Access Key ID |
| `secret_access_key: String` | AWS Secret Access Key |
| `session_token: Option<String>` | Optional STS session token |
| `S3Credentials::new(id, secret, session_token)` | Construct from explicit values |
| `S3Credentials::from_env()` | Load from `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` |
| `S3Credentials::from_env_with(getter)` | Load via a custom env getter (for testing) |

### `S3Config`

Full configuration (`Debug`, `Clone`).

| Field | Description |
|-------|-------------|
| `endpoint: String` | Endpoint URL (e.g. `https://s3.us-east-1.amazonaws.com` or `http://localhost:9000`) |
| `region: String` | AWS region |
| `bucket: String` | Bucket name |
| `credentials: S3Credentials` | Signing credentials |
| `path_style: bool` | `true` = `endpoint/bucket/key`; `false` = `bucket.host/key` |
| `timeout_secs: u64` | Per-operation timeout in seconds |
| `retry_config: RetryConfig` | Retry / backoff configuration |

### `S3BlobStoreBuilder`

`Default`. Builder for `S3Config` and the resulting store.

| Method | Description |
|--------|-------------|
| `S3BlobStoreBuilder::new()` | Defaults: path-style, 30 s timeout, default retry |
| `endpoint(e)` / `region(r)` / `bucket(b)` | Set required fields |
| `credentials(c)` | Set the credentials |
| `path_style(bool)` | Toggle path-style URLs (default `true`) |
| `timeout_secs(secs)` | Per-operation timeout (default 30) |
| `retry_config(cfg)` | Override retry/backoff |
| `build()` | Construct the `S3BlobStore` (errors if endpoint/region/bucket/credentials are unset) |

### `RetryConfig` (`retry` module)

`Debug`, `Clone`, `PartialEq`, `Eq`, `Default` (3 attempts, 100 ms base, 5000 ms cap). Truncated binary exponential backoff with deterministic per-attempt variance; HTTP 429/503 and transport errors are retried.

| Field / Function | Description |
|------------------|-------------|
| `max_attempts: u32` | Total attempts (1 = no retry) |
| `base_delay_ms: u64` | Base delay before the first retry |
| `max_delay_ms: u64` | Delay cap |
| `retry::backoff_delay(config, attempt)` | Compute the `Duration` for a 0-indexed retry attempt |
| `retry::should_retry_status(status)` | `true` for 429 / 503 |

### Error handling (`error` module)

| Item | Description |
|------|-------------|
| `S3ErrorResponse` | Parsed S3 XML error (`code`, `message`); `S3ErrorResponse::parse(xml) -> Option<Self>` |
| `http_error_to_blob_error(status, body, key)` | Map an HTTP status + body to a `BlobError` (404 `NoSuchKey`/`NoSuchBucket` → `NotFound`) |

Errors are surfaced as [`oxistore_blob::BlobError`](../oxistore-blob); transport/HTTP failures that exhaust retries become `BlobError::RetryExhausted`.

### SigV4 signing (`sigv4` module)

| Function | Description |
|----------|-------------|
| `sigv4::sign_request(method, uri, headers, body, credentials, region)` | Sign a request; returns the extra headers to inject (`x-amz-date`, `authorization`, and optionally `x-amz-security-token`) |

## Feature Flags

This crate defines no Cargo features.

## Cross-references

- [`oxistore-blob`](../oxistore-blob) — the `BlobStore` trait, `BlobMeta`, `Digest`, and `BlobError` this crate builds on.
- [`oxistore-blob-gcs`](../oxistore-blob-gcs) / [`oxistore-blob-azure`](../oxistore-blob-azure) — sibling cloud backends for Google Cloud Storage and Azure Blob Storage.
- [`oxistore-core`](../oxistore-core) / [`oxistore`](../oxistore) — the trait crate and top-level facade.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

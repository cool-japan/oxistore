//! `oxistore-blob-gcs` — Pure Rust Google Cloud Storage [`BlobStore`] backend.
//!
//! Implements the [`BlobStore`] trait against GCS using:
//! - RS256 JWT-based OAuth2 service account authentication (RFC 7515 / 7519)
//! - The GCS JSON API v1 over HTTPS via [`oxihttp_client`]
//! - Bearer token caching with automatic refresh
//!
//! # Authentication
//!
//! Provide a Google service account JSON key file.  The crate parses the
//! `client_email` and `private_key` fields, constructs a signed JWT, and
//! exchanges it for a short-lived Bearer token at the `token_uri` endpoint.
//! The token is cached in memory and refreshed automatically.
//!
//! # Example
//!
//! ```no_run
//! use oxistore_blob_gcs::{GcsBlobStore, GcsConfig, GcsServiceAccount};
//! use oxistore_blob::BlobStore;
//! use bytes::Bytes;
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), oxistore_blob::BlobError> {
//! let sa = GcsServiceAccount::from_env()?;
//! let config = GcsConfig {
//!     bucket: "my-bucket".to_string(),
//!     credentials: sa,
//!     timeout: Duration::from_secs(30),
//!     endpoint: None,
//!     oauth_endpoint: None,
//! };
//! let store = GcsBlobStore::new(config)?;
//! store.put("hello.txt", Bytes::from("hello GCS")).await?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod auth;
pub mod config;
pub mod error;

pub use auth::TokenCache;
pub use config::{GcsConfig, GcsServiceAccount};
pub use error::GcsError;

use bytes::Bytes;
use oxihttp_client::HttpsClient;
use oxistore_blob::{BlobError, BlobMeta, BlobStore};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

// ── GcsBlobStore ──────────────────────────────────────────────────────────────

/// GCS-backed blob store implementing [`BlobStore`].
///
/// Created via [`GcsBlobStore::new`].
pub struct GcsBlobStore {
    config: GcsConfig,
    client: HttpsClient,
    /// Inner token cache shared across async blocks (no `&self` lifetime in futures).
    token_inner: Arc<Mutex<Option<(String, Instant)>>>,
}

impl std::fmt::Debug for GcsBlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcsBlobStore")
            .field("bucket", &self.config.bucket)
            .field("endpoint", &self.config.endpoint)
            .finish()
    }
}

impl GcsBlobStore {
    /// Create a new `GcsBlobStore` from a [`GcsConfig`].
    ///
    /// Constructs an HTTPS client (backed by `oxihttp-client` with `tls` feature)
    /// that can talk to both `https://storage.googleapis.com` (production) and
    /// plain-HTTP test endpoints.
    pub fn new(config: GcsConfig) -> Result<Self, BlobError> {
        let client = oxihttp_client::Client::builder()
            .with_tls()
            .connect_timeout(config.timeout)
            .read_timeout(config.timeout)
            .build_https()
            .map_err(|e| BlobError::Other(format!("build HTTP client: {e}")))?;

        Ok(Self {
            config,
            client,
            token_inner: Arc::new(Mutex::new(None)),
        })
    }

    /// GCS-encode an object name for use in URL path segments.
    ///
    /// RFC 3986 percent-encoding; `/` is encoded so object names with slashes
    /// are treated as a single resource, not a path hierarchy.
    fn encode_key(key: &str) -> String {
        gcs_percent_encode(key)
    }

    /// Build the GCS JSON API URL for a specific object.
    ///
    /// `alt=media` yields the raw bytes; omitting it yields JSON metadata.
    fn object_url(&self, key: &str, alt_media: bool) -> String {
        let endpoint = self.config.storage_endpoint().trim_end_matches('/');
        let bucket = &self.config.bucket;
        let encoded = Self::encode_key(key);
        if alt_media {
            format!("{endpoint}/storage/v1/b/{bucket}/o/{encoded}?alt=media")
        } else {
            format!("{endpoint}/storage/v1/b/{bucket}/o/{encoded}")
        }
    }

    /// Build the GCS upload URL for a specific object.
    fn upload_url(&self, key: &str) -> String {
        let endpoint = self.config.storage_endpoint().trim_end_matches('/');
        let bucket = &self.config.bucket;
        let encoded = Self::encode_key(key);
        format!("{endpoint}/upload/storage/v1/b/{bucket}/o?uploadType=media&name={encoded}")
    }

    /// Build context needed by async operations without capturing `&self`.
    fn async_ctx(&self, key: &str) -> AsyncCtx {
        AsyncCtx {
            key: key.to_string(),
            client: self.client.clone(),
            token_inner: Arc::clone(&self.token_inner),
            sa: self.config.credentials.clone(),
            token_endpoint: self.config.token_endpoint().to_string(),
        }
    }

    /// Build context for list (no per-key URL needed).
    fn list_ctx(&self) -> AsyncCtx {
        AsyncCtx {
            key: String::new(),
            client: self.client.clone(),
            token_inner: Arc::clone(&self.token_inner),
            sa: self.config.credentials.clone(),
            token_endpoint: self.config.token_endpoint().to_string(),
        }
    }
}

/// Context cloned into async blocks to avoid `&self` lifetime issues.
struct AsyncCtx {
    key: String,
    client: HttpsClient,
    token_inner: Arc<Mutex<Option<(String, Instant)>>>,
    sa: GcsServiceAccount,
    token_endpoint: String,
}

impl AsyncCtx {
    async fn bearer_token(&self) -> Result<String, BlobError> {
        acquire_token(
            &self.token_inner,
            &self.sa,
            &self.client,
            &self.token_endpoint,
        )
        .await
    }
}

// ── BlobStore implementation ──────────────────────────────────────────────────

impl BlobStore for GcsBlobStore {
    fn put(&self, key: &str, data: Bytes) -> impl Future<Output = Result<(), BlobError>> + Send {
        let url = self.upload_url(key);
        let ctx = self.async_ctx(key);
        let data_vec = data.to_vec();

        async move {
            let token = ctx.bearer_token().await?;

            let resp = ctx
                .client
                .post(&url)
                .map_err(|e| BlobError::Other(format!("GCS put build request: {e}")))?
                .bearer_token(&token)
                .map_err(|e| BlobError::Other(format!("GCS put set auth: {e}")))?
                .header("Content-Type", "application/octet-stream")
                .map_err(|e| BlobError::Other(format!("GCS put set content-type: {e}")))?
                .body(data_vec)
                .send()
                .await
                .map_err(|e| BlobError::Other(format!("GCS put send: {e}")))?;

            let status = resp.status().as_u16();
            if status == 200 || status == 201 {
                Ok(())
            } else {
                let body = resp
                    .body_bytes()
                    .await
                    .map_err(|e| BlobError::Other(format!("GCS put read body: {e}")))?;
                Err(gcs_status_to_blob_error(status, &ctx.key, &body))
            }
        }
    }

    fn get(&self, key: &str) -> impl Future<Output = Result<Bytes, BlobError>> + Send {
        let url = self.object_url(key, true);
        let ctx = self.async_ctx(key);

        async move {
            let token = ctx.bearer_token().await?;

            let resp = ctx
                .client
                .get(&url)
                .map_err(|e| BlobError::Other(format!("GCS get build request: {e}")))?
                .bearer_token(&token)
                .map_err(|e| BlobError::Other(format!("GCS get set auth: {e}")))?
                .send()
                .await
                .map_err(|e| BlobError::Other(format!("GCS get send: {e}")))?;

            let status = resp.status().as_u16();
            if status == 200 {
                resp.body_bytes()
                    .await
                    .map_err(|e| BlobError::Other(format!("GCS get read body: {e}")))
            } else {
                let body = resp
                    .body_bytes()
                    .await
                    .map_err(|e| BlobError::Other(format!("GCS get read err body: {e}")))?;
                Err(gcs_status_to_blob_error(status, &ctx.key, &body))
            }
        }
    }

    fn delete(&self, key: &str) -> impl Future<Output = Result<(), BlobError>> + Send {
        let url = self.object_url(key, false);
        let ctx = self.async_ctx(key);

        async move {
            let token = ctx.bearer_token().await?;

            let resp = ctx
                .client
                .delete(&url)
                .map_err(|e| BlobError::Other(format!("GCS delete build request: {e}")))?
                .bearer_token(&token)
                .map_err(|e| BlobError::Other(format!("GCS delete set auth: {e}")))?
                .send()
                .await
                .map_err(|e| BlobError::Other(format!("GCS delete send: {e}")))?;

            let status = resp.status().as_u16();
            match status {
                200 | 204 => Ok(()),
                _ => {
                    let body = resp
                        .body_bytes()
                        .await
                        .map_err(|e| BlobError::Other(format!("GCS delete read body: {e}")))?;
                    Err(gcs_status_to_blob_error(status, &ctx.key, &body))
                }
            }
        }
    }

    fn head(&self, key: &str) -> impl Future<Output = Result<BlobMeta, BlobError>> + Send {
        let url = self.object_url(key, false);
        let ctx = self.async_ctx(key);

        async move {
            let token = ctx.bearer_token().await?;

            let resp = ctx
                .client
                .get(&url)
                .map_err(|e| BlobError::Other(format!("GCS head build request: {e}")))?
                .bearer_token(&token)
                .map_err(|e| BlobError::Other(format!("GCS head set auth: {e}")))?
                .send()
                .await
                .map_err(|e| BlobError::Other(format!("GCS head send: {e}")))?;

            let status = resp.status().as_u16();
            let body = resp
                .body_bytes()
                .await
                .map_err(|e| BlobError::Other(format!("GCS head read body: {e}")))?;

            if status == 200 {
                parse_object_metadata(&ctx.key, &body)
            } else {
                Err(gcs_status_to_blob_error(status, &ctx.key, &body))
            }
        }
    }

    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, BlobError>> + Send {
        let endpoint = self
            .config
            .storage_endpoint()
            .trim_end_matches('/')
            .to_string();
        let bucket = self.config.bucket.clone();
        let prefix = prefix.to_string();
        let ctx = self.list_ctx();

        async move {
            let token = ctx.bearer_token().await?;
            let mut keys: Vec<String> = Vec::new();
            let mut page_token: Option<String> = None;

            loop {
                let url = build_list_url(&endpoint, &bucket, &prefix, page_token.as_deref());

                let resp = ctx
                    .client
                    .get(&url)
                    .map_err(|e| BlobError::Other(format!("GCS list build request: {e}")))?
                    .bearer_token(&token)
                    .map_err(|e| BlobError::Other(format!("GCS list set auth: {e}")))?
                    .send()
                    .await
                    .map_err(|e| BlobError::Other(format!("GCS list send: {e}")))?;

                let status = resp.status().as_u16();
                let body = resp
                    .body_bytes()
                    .await
                    .map_err(|e| BlobError::Other(format!("GCS list read body: {e}")))?;

                if status != 200 {
                    return Err(BlobError::Other(format!(
                        "GCS list returned {status}: {}",
                        String::from_utf8_lossy(&body)
                    )));
                }

                let page = parse_list_page(&body)?;
                keys.extend(page.keys);

                match page.next_page_token {
                    Some(t) if !t.is_empty() => {
                        page_token = Some(t);
                    }
                    _ => break,
                }
            }

            keys.sort();
            Ok(keys)
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Acquire a cached or fresh Bearer token.
///
/// Takes the inner `Arc<Mutex<...>>` directly so that the async block that
/// calls this does not need a reference to `&self`.
async fn acquire_token(
    cache: &Arc<Mutex<Option<(String, Instant)>>>,
    sa: &GcsServiceAccount,
    client: &HttpsClient,
    token_uri: &str,
) -> Result<String, BlobError> {
    let mut guard = cache.lock().await;
    if let Some((ref token, expiry)) = *guard {
        if Instant::now() + Duration::from_secs(60) < expiry {
            return Ok(token.clone());
        }
    }

    // Build JWT and exchange for a Bearer token
    let jwt = auth::build_jwt(sa, token_uri).map_err(BlobError::from)?;

    let form_body =
        format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={jwt}");

    let resp = client
        .post(token_uri)
        .map_err(|e| BlobError::Other(format!("token POST build: {e}")))?
        .header("Content-Type", "application/x-www-form-urlencoded")
        .map_err(|e| BlobError::Other(format!("token POST content-type: {e}")))?
        .body(form_body.into_bytes())
        .send()
        .await
        .map_err(|e| BlobError::Other(format!("token POST send: {e}")))?;

    let status = resp.status().as_u16();
    let body = resp
        .body_bytes()
        .await
        .map_err(|e| BlobError::Other(format!("token POST read body: {e}")))?;

    if status != 200 {
        return Err(BlobError::Other(format!(
            "token endpoint returned {status}: {}",
            String::from_utf8_lossy(&body)
        )));
    }

    #[derive(serde::Deserialize)]
    struct TokenResp {
        access_token: String,
        expires_in: Option<u64>,
    }

    let tr: TokenResp = serde_json::from_slice(&body)
        .map_err(|e| BlobError::Other(format!("token response parse: {e}")))?;

    let ttl = tr.expires_in.unwrap_or(3600);
    let expiry = Instant::now() + Duration::from_secs(ttl);
    *guard = Some((tr.access_token.clone(), expiry));
    Ok(tr.access_token)
}

/// Convert an HTTP status code from GCS to a [`BlobError`].
fn gcs_status_to_blob_error(status: u16, key: &str, body: &[u8]) -> BlobError {
    match status {
        404 => BlobError::NotFound(key.to_string()),
        401 | 403 => BlobError::Other(format!(
            "gcs auth: permission denied ({status}): {}",
            String::from_utf8_lossy(body)
        )),
        _ => BlobError::Other(format!(
            "gcs http {status} for key {key:?}: {}",
            String::from_utf8_lossy(body)
        )),
    }
}

/// Parse a GCS object metadata JSON response into a [`BlobMeta`].
fn parse_object_metadata(key: &str, body: &[u8]) -> Result<BlobMeta, BlobError> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| BlobError::Other(format!("GCS metadata parse: {e}")))?;

    let size: u64 = v["size"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v["size"].as_u64())
        .unwrap_or(0);

    let content_type = v["contentType"].as_str().map(str::to_string);

    let mut meta = BlobMeta::new(key, size);
    meta.content_type = content_type;
    Ok(meta)
}

/// A single page from the GCS list objects response.
struct ListPage {
    keys: Vec<String>,
    next_page_token: Option<String>,
}

/// Percent-encode all non-unreserved characters (RFC 3986 §2.3).
///
/// This is stricter than `form_urlencoded` — it encodes `/` as `%2F`,
/// which is required for GCS object names in URL path segments.
fn gcs_percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            // Unreserved characters (RFC 3986 §2.3): A–Z a–z 0–9 - _ . ~
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            other => {
                out.push('%');
                out.push(
                    char::from_digit(u32::from(other >> 4), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit(u32::from(other & 0xf), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    out
}

/// Build the GCS storage list URL.
fn build_list_url(endpoint: &str, bucket: &str, prefix: &str, page_token: Option<&str>) -> String {
    let encoded_prefix = gcs_percent_encode(prefix);
    let mut url =
        format!("{endpoint}/storage/v1/b/{bucket}/o?maxResults=1000&prefix={encoded_prefix}");
    if let Some(token) = page_token {
        let encoded_token = gcs_percent_encode(token);
        url.push_str(&format!("&pageToken={encoded_token}"));
    }
    url
}

/// Parse a GCS `storage/v1/b/{bucket}/o` list response.
fn parse_list_page(body: &[u8]) -> Result<ListPage, BlobError> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| BlobError::Other(format!("GCS list parse: {e}")))?;

    let items = match v.get("items").and_then(|i| i.as_array()) {
        Some(arr) => arr,
        None => {
            // Empty prefix match or no items returns `{}` with no `items` key
            return Ok(ListPage {
                keys: Vec::new(),
                next_page_token: None,
            });
        }
    };

    let keys: Vec<String> = items
        .iter()
        .filter_map(|item| item["name"].as_str().map(str::to_string))
        .collect();

    let next_page_token = v["nextPageToken"].as_str().map(str::to_string);

    Ok(ListPage {
        keys,
        next_page_token,
    })
}

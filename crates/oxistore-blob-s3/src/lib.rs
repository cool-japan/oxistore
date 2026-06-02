//! `oxistore-blob-s3` — S3-compatible [`BlobStore`] backend.
//!
//! Implements the [`BlobStore`] trait against any S3-compatible endpoint
//! (AWS S3, MinIO, Ceph RGW, DigitalOcean Spaces, etc.).
//!
//! # Authentication
//!
//! Requests are signed with AWS Signature Version 4 (SigV4) using the
//! [`aws_sigv4`] crate.  The signing closure is Pure Rust — **no ring, no
//! aws-lc-rs** in the default dependency graph (verified 2026-05-27).
//!
//! # HTTP transport
//!
//! Uses `oxihttp_client::Client` (hyper-based) with the `tls` feature enabled
//! workspace-wide.  A single `HttpsClient` instance handles both `http://`
//! (MinIO / test mocks) and `https://` (real AWS S3) endpoints.
//!
//! # Example
//!
//! ```no_run
//! use oxistore_blob_s3::{S3BlobStore, S3BlobStoreBuilder, S3Credentials};
//! use oxistore_blob::BlobStore;
//! use bytes::Bytes;
//!
//! # async fn example() -> Result<(), oxistore_blob::BlobError> {
//! let store = S3BlobStoreBuilder::new()
//!     .endpoint("http://localhost:9000")
//!     .region("us-east-1")
//!     .bucket("mybucket")
//!     .credentials(S3Credentials::from_env()?)
//!     .path_style(true)
//!     .build()?;
//!
//! store.put("readme.txt", Bytes::from("hello S3")).await?;
//! let data = store.get("readme.txt").await?;
//! assert_eq!(data.as_ref(), b"hello S3");
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod multipart;
pub mod presign;
pub mod retry;
pub mod sigv4;

pub use config::{S3BlobStoreBuilder, S3Config, S3Credentials};
pub use error::{http_error_to_blob_error, S3ErrorResponse};
pub use multipart::S3MultipartUpload;
pub use retry::RetryConfig;

use bytes::Bytes;
use oxihttp_client::{ClientBuilder, HttpsClient};
use oxistore_blob::{BlobError, BlobMeta, BlobStore};
use std::future::Future;
use url::Url;

/// Internal response struct (mirrors `http_client::RawResponse` for migration).
pub(crate) struct InternalResponse {
    pub(crate) status: u16,
    pub(crate) headers: std::collections::HashMap<String, String>,
    pub(crate) body: Bytes,
}

/// S3-compatible blob store implementing [`BlobStore`].
///
/// Created via [`S3BlobStoreBuilder`].
#[derive(Clone)]
pub struct S3BlobStore {
    /// S3 configuration (endpoint, region, bucket, credentials, retry config).
    pub(crate) config: S3Config,
    /// Shared HTTP client (connection-pooled, handles both http:// and https://).
    pub(crate) client: HttpsClient,
}

impl std::fmt::Debug for S3BlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3BlobStore")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl S3BlobStore {
    /// Construct directly from a complete [`S3Config`].
    ///
    /// Prefer [`S3BlobStoreBuilder`] for a more ergonomic API.
    pub fn new(config: S3Config) -> Result<Self, BlobError> {
        let timeout = std::time::Duration::from_secs(config.timeout_secs);
        let client = ClientBuilder::new()
            .with_tls()
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .build_https()
            .map_err(|e| BlobError::Other(format!("oxihttp-client build failed: {e}")))?;
        Ok(Self { config, client })
    }

    /// Return the base URL for the bucket (without trailing slash).
    pub(crate) fn bucket_url(&self) -> Result<String, BlobError> {
        let endpoint = self.config.endpoint.trim_end_matches('/');
        if self.config.path_style {
            Ok(format!("{endpoint}/{}", self.config.bucket))
        } else {
            let parsed = Url::parse(endpoint)
                .map_err(|e| BlobError::Other(format!("invalid endpoint URL: {e}")))?;
            let scheme = parsed.scheme();
            let host = parsed
                .host_str()
                .ok_or_else(|| BlobError::Other("endpoint URL missing host".to_string()))?;
            let port_suffix = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
            Ok(format!(
                "{scheme}://{}.{host}{port_suffix}",
                self.config.bucket
            ))
        }
    }

    /// Return the endpoint URL for the given object key.
    ///
    /// Supports both path-style and virtual-host-style URLs.
    pub(crate) fn object_url(&self, key: &str) -> Result<String, BlobError> {
        let base = self.bucket_url()?;
        Ok(format!("{base}/{key}"))
    }

    /// Extract host:port from a URL string.
    pub(crate) fn url_host_header(url: &str) -> Result<String, BlobError> {
        let parsed =
            Url::parse(url).map_err(|e| BlobError::Other(format!("parse URL {url:?}: {e}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| BlobError::Other(format!("URL has no host: {url}")))?
            .to_string();
        let port = parsed.port().unwrap_or_else(|| match parsed.scheme() {
            "https" => 443,
            _ => 80,
        });
        Ok(format!("{host}:{port}"))
    }

    /// Send a signed HTTP request using `oxihttp_client` with retry/backoff.
    ///
    /// Returns an [`InternalResponse`] with status, headers (lowercased), and body.
    pub(crate) async fn send(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
        extra_headers: &[(&str, &str)],
    ) -> Result<InternalResponse, BlobError> {
        let retry = &self.config.retry_config;
        let max_attempts = retry.max_attempts;

        let mut last_error: Option<BlobError> = None;

        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = retry::backoff_delay(retry, attempt - 1);
                tokio::time::sleep(delay).await;
            }

            match self.send_once(method, url, body, extra_headers).await {
                Ok(resp) if retry::should_retry_status(resp.status) => {
                    // 503 / 429 — retry
                    last_error = Some(BlobError::Other(format!(
                        "S3 HTTP {} (retryable)",
                        resp.status
                    )));
                    continue;
                }
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    // transport errors are retried
                    last_error = Some(e);
                    continue;
                }
            }
        }

        let last = last_error.unwrap_or_else(|| BlobError::Other("unknown error".to_string()));
        Err(BlobError::RetryExhausted {
            attempts: max_attempts,
            last_error: last.to_string(),
        })
    }

    /// Single HTTP request attempt (no retry).
    async fn send_once(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
        extra_headers: &[(&str, &str)],
    ) -> Result<InternalResponse, BlobError> {
        let host_header = Self::url_host_header(url)?;

        // Build header list for signing: host first, then extras
        let mut sign_headers: Vec<(&str, &str)> = vec![("host", &host_header)];
        for (k, v) in extra_headers {
            sign_headers.push((k, v));
        }

        // Sign the request
        let signed_headers = sigv4::sign_request(
            method,
            url,
            &sign_headers,
            body,
            &self.config.credentials,
            &self.config.region,
        )?;

        // Collect all headers (sign_headers + signed additions)
        let mut all_headers: Vec<(String, String)> = sign_headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        all_headers.extend(signed_headers);

        // Build the request via oxihttp_client
        let rb = match method {
            "GET" => self.client.get(url),
            "PUT" => self.client.put(url),
            "POST" => self.client.post(url),
            "DELETE" => self.client.delete(url),
            "HEAD" => self.client.head(url),
            "PATCH" => self.client.patch(url),
            other => {
                return Err(BlobError::Other(format!(
                    "unsupported HTTP method: {other}"
                )))
            }
        }
        .map_err(|e| BlobError::Other(format!("request builder: {e}")))?;

        // Inject all headers
        let mut rb = rb;
        for (k, v) in &all_headers {
            rb = rb
                .header(k.as_str(), v.as_str())
                .map_err(|e| BlobError::Other(format!("set header {k}: {e}")))?;
        }

        // Set body
        let rb = rb.body(Bytes::copy_from_slice(body));

        // Send
        let resp = rb
            .send()
            .await
            .map_err(|e| BlobError::Other(format!("HTTP {method} {url}: {e}")))?;

        let status = resp.status().as_u16();

        // Collect headers before consuming body
        let resp_headers: std::collections::HashMap<String, String> = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_lowercase(),
                    v.to_str().unwrap_or("").to_string(),
                )
            })
            .collect();

        // For HEAD requests the body is always empty
        let body_bytes = if method == "HEAD" {
            Bytes::new()
        } else {
            resp.body_bytes()
                .await
                .map_err(|e| BlobError::Other(format!("read response body: {e}")))?
        };

        Ok(InternalResponse {
            status,
            headers: resp_headers,
            body: body_bytes,
        })
    }

    /// Server-side copy: PUT `/<dst>` with `x-amz-copy-source: /<bucket>/<src>`.
    ///
    /// No data is transferred through the client — S3 copies server-side.
    pub async fn copy(&self, src: &str, dst: &str) -> Result<(), BlobError> {
        let copy_source = format!("/{}/{src}", self.config.bucket);
        let dst_url = self.object_url(dst)?;
        let extra = [("x-amz-copy-source", copy_source.as_str())];
        let resp = self.send("PUT", &dst_url, &[], &extra).await?;
        match resp.status {
            200 | 204 => Ok(()),
            _ => Err(http_error_to_blob_error(resp.status, &resp.body, dst)),
        }
    }
}

// ── BlobStore implementation ──────────────────────────────────────────────────

impl BlobStore for S3BlobStore {
    fn put(&self, key: &str, data: Bytes) -> impl Future<Output = Result<(), BlobError>> + Send {
        let key = key.to_string();
        let data_vec = data.to_vec();
        async move {
            let url = self.object_url(&key)?;
            let resp = self.send("PUT", &url, &data_vec, &[]).await?;
            if resp.status == 200 || resp.status == 204 {
                Ok(())
            } else {
                Err(http_error_to_blob_error(resp.status, &resp.body, &key))
            }
        }
    }

    fn get(&self, key: &str) -> impl Future<Output = Result<Bytes, BlobError>> + Send {
        let key = key.to_string();
        async move {
            let url = self.object_url(&key)?;
            let resp = self.send("GET", &url, &[], &[]).await?;
            if resp.status == 200 {
                Ok(resp.body)
            } else {
                Err(http_error_to_blob_error(resp.status, &resp.body, &key))
            }
        }
    }

    fn delete(&self, key: &str) -> impl Future<Output = Result<(), BlobError>> + Send {
        let key = key.to_string();
        async move {
            let url = self.object_url(&key)?;
            let resp = self.send("DELETE", &url, &[], &[]).await?;
            match resp.status {
                200 | 204 => Ok(()),
                404 => Err(BlobError::NotFound(key)),
                _ => Err(http_error_to_blob_error(resp.status, &resp.body, &key)),
            }
        }
    }

    fn head(&self, key: &str) -> impl Future<Output = Result<BlobMeta, BlobError>> + Send {
        let key = key.to_string();
        async move {
            let url = self.object_url(&key)?;
            let resp = self.send("HEAD", &url, &[], &[]).await?;
            match resp.status {
                200 => {
                    let size: u64 = resp
                        .headers
                        .get("content-length")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let content_type = resp.headers.get("content-type").cloned();
                    let mut meta = BlobMeta::new(key.clone(), size);
                    meta.content_type = content_type;
                    Ok(meta)
                }
                404 => Err(BlobError::NotFound(key)),
                _ => Err(http_error_to_blob_error(resp.status, &resp.body, &key)),
            }
        }
    }

    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, BlobError>> + Send {
        let prefix = prefix.to_string();
        let config = self.config.clone();
        async move { list_all_pages(self, &config, &prefix).await }
    }
}

/// Paginated ListObjectsV2 — loops until IsTruncated is false.
async fn list_all_pages(
    store: &S3BlobStore,
    config: &S3Config,
    prefix: &str,
) -> Result<Vec<String>, BlobError> {
    let mut all_keys: Vec<String> = Vec::new();
    let mut continuation_token: Option<String> = None;

    loop {
        let url = build_list_url(config, prefix, continuation_token.as_deref())?;
        let resp = store.send("GET", &url, &[], &[]).await?;

        if resp.status != 200 {
            return Err(http_error_to_blob_error(resp.status, &resp.body, prefix));
        }

        let page = parse_list_objects_v2_page(&resp.body)?;
        all_keys.extend(page.keys);

        if page.is_truncated {
            continuation_token = page.next_continuation_token;
            if continuation_token.is_none() {
                // Malformed response; stop to avoid infinite loop
                break;
            }
        } else {
            break;
        }
    }

    Ok(all_keys)
}

/// Build the ListObjectsV2 URL with optional continuation token.
fn build_list_url(
    config: &S3Config,
    prefix: &str,
    continuation_token: Option<&str>,
) -> Result<String, BlobError> {
    let endpoint = config.endpoint.trim_end_matches('/');
    let base = if config.path_style {
        format!("{endpoint}/{}?list-type=2", config.bucket)
    } else {
        let parsed = Url::parse(endpoint)
            .map_err(|e| BlobError::Other(format!("invalid endpoint URL: {e}")))?;
        let scheme = parsed.scheme();
        let host = parsed
            .host_str()
            .ok_or_else(|| BlobError::Other("endpoint URL missing host".to_string()))?;
        let port_suffix = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
        format!(
            "{scheme}://{}.{host}{port_suffix}/?list-type=2",
            config.bucket
        )
    };

    let mut url = base;
    if !prefix.is_empty() {
        url.push_str(&format!("&prefix={}", percent_encode(prefix)));
    }
    if let Some(token) = continuation_token {
        url.push_str(&format!("&continuation-token={}", percent_encode(token)));
    }
    Ok(url)
}

/// Parsed page from ListObjectsV2.
struct ListPage {
    keys: Vec<String>,
    is_truncated: bool,
    next_continuation_token: Option<String>,
}

/// Parse a ListObjectsV2 XML response page (supports pagination).
fn parse_list_objects_v2_page(xml: &[u8]) -> Result<ListPage, BlobError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut keys = Vec::new();
    let mut is_truncated = false;
    let mut next_continuation_token: Option<String> = None;
    let mut current_tag = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                current_tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
            }
            Ok(Event::Text(e)) => {
                let text = e
                    .decode()
                    .map_err(|err| BlobError::Other(format!("XML decode error: {err}")))?
                    .into_owned();
                match current_tag.as_str() {
                    "Key" => keys.push(text),
                    "IsTruncated" => {
                        is_truncated = text.eq_ignore_ascii_case("true");
                    }
                    "NextContinuationToken" => {
                        next_continuation_token = Some(text);
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(BlobError::Other(format!(
                    "XML parse error in ListObjectsV2 response: {e}"
                )))
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(ListPage {
        keys,
        is_truncated,
        next_continuation_token,
    })
}

/// Percent-encode a string for use as a URL query parameter value.
pub(crate) fn percent_encode(s: &str) -> String {
    s.bytes()
        .flat_map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                vec![b as char]
            }
            other => format!("%{other:02X}").chars().collect(),
        })
        .collect()
}

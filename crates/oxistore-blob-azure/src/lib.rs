//! Pure-Rust Azure Blob Storage BlobStore adapter.
//!
//! Implements the [`BlobStore`] trait using Azure's Shared Key v2 HMAC-SHA256
//! authentication.  No native code, no `ring` — 100% Pure Rust.
//!
//! # Example
//!
//! ```no_run
//! use oxistore_blob_azure::{AzureBlobStore, AzureConfig, AzureCredentials};
//!
//! # async fn example() -> Result<(), oxistore_blob::BlobError> {
//! let creds = AzureCredentials::from_env()?;
//! let config = AzureConfig::new(creds, "my-container");
//! let store = AzureBlobStore::new(config)?;
//! // Use store like any other BlobStore …
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub(crate) mod sign;

pub use config::{AzureConfig, AzureCredentials};
pub use error::AzureError;
pub use sign::SharedKeySigner;

use bytes::Bytes;
use oxistore_blob::{BlobError, BlobMeta, BlobStore};
use sign::rfc1123_now;
use std::future::Future;

// ── AzureBlobStore ────────────────────────────────────────────────────────────

/// Azure Blob Storage backend implementing [`BlobStore`].
///
/// Uses Azure Shared Key v2 (HMAC-SHA256) for authentication.
pub struct AzureBlobStore {
    config: AzureConfig,
    client: oxihttp_client::Client,
    signer: SharedKeySigner,
}

impl AzureBlobStore {
    /// Construct a new `AzureBlobStore` from the given configuration.
    pub fn new(config: AzureConfig) -> Result<Self, BlobError> {
        let key_bytes = config.credentials.key_bytes().map_err(BlobError::from)?;
        let signer = SharedKeySigner::new(config.credentials.account_name.clone(), key_bytes);
        let client = oxihttp_client::Client::builder()
            .build()
            .map_err(|e| BlobError::Other(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            config,
            client,
            signer,
        })
    }

    /// Base endpoint URL (no trailing slash).
    fn endpoint(&self) -> String {
        self.config.endpoint.clone().unwrap_or_else(|| {
            format!(
                "https://{}.blob.core.windows.net",
                self.config.credentials.account_name
            )
        })
    }

    /// Full URL for a blob.
    ///
    /// `key` is percent-encoded per path segment (preserving `/` as the
    /// segment separator) via [`percent_encode`], so keys containing `?`,
    /// `#`, spaces, or non-ASCII characters produce a well-formed URL — a
    /// raw `?`/`#` in an un-encoded key would otherwise be reinterpreted as
    /// the start of the query string / fragment, silently truncating the
    /// path or targeting the wrong blob.
    fn blob_url(&self, key: &str) -> String {
        format!(
            "{}/{}/{}",
            self.endpoint(),
            self.config.container,
            percent_encode(key)
        )
    }

    /// Build the standard x-ms-* headers for every request.
    ///
    /// Returns `(date, version)` values — callers add more headers as needed.
    fn base_headers(&self) -> (String, &'static str) {
        (rfc1123_now(), "2024-08-04")
    }

    /// Sign a request and return the Authorization header value.
    fn auth_header(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        content_length: Option<u64>,
        content_type: Option<&str>,
    ) -> Result<String, BlobError> {
        self.signer
            .sign(method, url, headers, content_length, content_type)
            .map_err(BlobError::from)
    }
}

// ── BlobStore impl ────────────────────────────────────────────────────────────

impl BlobStore for AzureBlobStore {
    fn put(&self, key: &str, data: Bytes) -> impl Future<Output = Result<(), BlobError>> + Send {
        let url = self.blob_url(key);
        let (date, version) = self.base_headers();
        let content_len = data.len() as u64;
        let content_type = "application/octet-stream";

        // Build sign headers slice including Content-Type.
        let sign_headers: &[(&str, &str)] = &[
            ("x-ms-date", &date),
            ("x-ms-version", version),
            ("x-ms-blob-type", "BlockBlob"),
            ("Content-Type", content_type),
        ];

        let auth = self.auth_header(
            "PUT",
            &url,
            sign_headers,
            Some(content_len),
            Some(content_type),
        );
        let client = self.client.clone();
        let timeout = self.config.timeout;

        async move {
            let auth = auth?;
            let resp = client
                .put(&url)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("x-ms-date", &date)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("x-ms-version", version)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("x-ms-blob-type", "BlockBlob")
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("Content-Type", content_type)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("Content-Length", &content_len.to_string())
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("Authorization", &auth)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .body(data)
                .timeout(timeout)
                .send()
                .await
                .map_err(|e| BlobError::Other(e.to_string()))?;

            let status = resp.status().as_u16();
            if status == 201 || status == 200 {
                Ok(())
            } else {
                Err(BlobError::Other(format!(
                    "azure PUT failed with status {status}"
                )))
            }
        }
    }

    fn get(&self, key: &str) -> impl Future<Output = Result<Bytes, BlobError>> + Send {
        let url = self.blob_url(key);
        let key_owned = key.to_string();
        let (date, version) = self.base_headers();

        let sign_headers: &[(&str, &str)] = &[("x-ms-date", &date), ("x-ms-version", version)];

        let auth = self.auth_header("GET", &url, sign_headers, None, None);
        let client = self.client.clone();
        let timeout = self.config.timeout;

        async move {
            let auth = auth?;
            let resp = client
                .get(&url)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("x-ms-date", &date)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("x-ms-version", version)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("Authorization", &auth)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .timeout(timeout)
                .send()
                .await
                .map_err(|e| BlobError::Other(e.to_string()))?;

            let status = resp.status().as_u16();
            if status == 404 {
                return Err(BlobError::NotFound(key_owned));
            }
            if status != 200 {
                return Err(BlobError::Other(format!(
                    "azure GET failed with status {status}"
                )));
            }
            resp.body_bytes()
                .await
                .map_err(|e| BlobError::Other(e.to_string()))
        }
    }

    fn delete(&self, key: &str) -> impl Future<Output = Result<(), BlobError>> + Send {
        let url = self.blob_url(key);
        let key_owned = key.to_string();
        let (date, version) = self.base_headers();

        let sign_headers: &[(&str, &str)] = &[("x-ms-date", &date), ("x-ms-version", version)];

        let auth = self.auth_header("DELETE", &url, sign_headers, None, None);
        let client = self.client.clone();
        let timeout = self.config.timeout;

        async move {
            let auth = auth?;
            let resp = client
                .delete(&url)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("x-ms-date", &date)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("x-ms-version", version)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("Authorization", &auth)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .timeout(timeout)
                .send()
                .await
                .map_err(|e| BlobError::Other(e.to_string()))?;

            let status = resp.status().as_u16();
            if status == 404 {
                return Err(BlobError::NotFound(key_owned));
            }
            if status == 202 || status == 200 || status == 204 {
                Ok(())
            } else {
                Err(BlobError::Other(format!(
                    "azure DELETE failed with status {status}"
                )))
            }
        }
    }

    fn head(&self, key: &str) -> impl Future<Output = Result<BlobMeta, BlobError>> + Send {
        let url = self.blob_url(key);
        let key_owned = key.to_string();
        let (date, version) = self.base_headers();

        let sign_headers: &[(&str, &str)] = &[("x-ms-date", &date), ("x-ms-version", version)];

        let auth = self.auth_header("HEAD", &url, sign_headers, None, None);
        let client = self.client.clone();
        let timeout = self.config.timeout;

        async move {
            let auth = auth?;
            let resp = client
                .head(&url)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("x-ms-date", &date)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("x-ms-version", version)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .header("Authorization", &auth)
                .map_err(|e| BlobError::Other(e.to_string()))?
                .timeout(timeout)
                .send()
                .await
                .map_err(|e| BlobError::Other(e.to_string()))?;

            let status = resp.status().as_u16();
            if status == 404 {
                return Err(BlobError::NotFound(key_owned.clone()));
            }
            if status != 200 {
                return Err(BlobError::Other(format!(
                    "azure HEAD failed with status {status}"
                )));
            }

            let size = resp.content_length().unwrap_or(0);
            let content_type = resp.content_type().map(str::to_string);

            let mut meta = BlobMeta::new(key_owned, size);
            meta.content_type = content_type;
            Ok(meta)
        }
    }

    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, BlobError>> + Send {
        let endpoint = self.endpoint();
        let container = self.config.container.clone();
        let prefix_owned = prefix.to_string();
        let (_, version) = self.base_headers();
        let client = self.client.clone();
        let timeout = self.config.timeout;
        let account_name = self.config.credentials.account_name.clone();
        let signer = self.signer.clone();

        async move {
            let mut blobs: Vec<String> = Vec::new();
            let mut marker: Option<String> = None;

            loop {
                let mut list_url = format!(
                    "{}/{}?restype=container&comp=list&prefix={}",
                    endpoint,
                    container,
                    percent_encode(&prefix_owned)
                );
                if let Some(ref m) = marker {
                    list_url.push_str("&marker=");
                    list_url.push_str(&percent_encode(m));
                }

                let list_date = rfc1123_now();
                let sign_headers: &[(&str, &str)] =
                    &[("x-ms-date", &list_date), ("x-ms-version", version)];

                let auth = signer
                    .sign("GET", &list_url, sign_headers, None, None)
                    .map_err(BlobError::from)?;

                let resp = client
                    .get(&list_url)
                    .map_err(|e| BlobError::Other(e.to_string()))?
                    .header("x-ms-date", &list_date)
                    .map_err(|e| BlobError::Other(e.to_string()))?
                    .header("x-ms-version", version)
                    .map_err(|e| BlobError::Other(e.to_string()))?
                    .header("Authorization", &auth)
                    .map_err(|e| BlobError::Other(e.to_string()))?
                    .timeout(timeout)
                    .send()
                    .await
                    .map_err(|e| BlobError::Other(e.to_string()))?;

                let status = resp.status().as_u16();
                if status != 200 {
                    return Err(BlobError::Other(format!(
                        "azure list failed with status {status} (account={account_name}, container={container})"
                    )));
                }

                let body = resp
                    .body_bytes()
                    .await
                    .map_err(|e| BlobError::Other(e.to_string()))?;

                let (page_blobs, next_marker) = parse_list_response(&body)?;
                blobs.extend(page_blobs);

                match next_marker {
                    Some(m) if !m.is_empty() => marker = Some(m),
                    _ => break,
                }
            }

            Ok(blobs)
        }
    }
}

// ── XML parsing ───────────────────────────────────────────────────────────────

/// Parse an Azure Blob Storage `ListBlobs` (`EnumerationResults`) XML
/// response body, returning `(blob_names, next_marker)`.
///
/// `body` is untrusted network input — the response of a real Azure Blob
/// Storage (or Azurite-emulated) `?restype=container&comp=list` request.
/// Public (beyond `pub(crate)`) so it can be exercised directly by
/// `fuzz/fuzz_targets/azure_list_response_xml.rs` without constructing a
/// live `AzureBlobStore` and HTTP round trip.
///
/// # Errors
///
/// Returns [`BlobError::Other`] if `body` is not well-formed XML. Malformed
/// or unexpected-but-well-formed XML (e.g. missing `Name`/`NextMarker`
/// elements) is tolerated and simply yields fewer results, rather than
/// rejecting the whole response over one missing optional field.
pub fn parse_list_response(body: &[u8]) -> Result<(Vec<String>, Option<String>), BlobError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);

    let mut names: Vec<String> = Vec::new();
    let mut next_marker: Option<String> = None;
    let mut current_tag = String::new();
    let mut in_blob = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name_bytes = e.name();
                let tag = std::str::from_utf8(name_bytes.as_ref())
                    .unwrap_or("")
                    .to_string();
                if tag == "Blob" {
                    in_blob = true;
                }
                current_tag = tag;
            }
            Ok(Event::End(ref e)) => {
                let name_bytes = e.name();
                let raw = name_bytes.as_ref();
                let tag = std::str::from_utf8(raw).unwrap_or("");
                if tag == "Blob" {
                    in_blob = false;
                }
                current_tag.clear();
            }
            Ok(Event::Text(ref e)) => {
                let text = e
                    .decode()
                    .map_err(|err| BlobError::Other(format!("xml decode error: {err}")))?
                    .to_string();

                if in_blob && current_tag == "Name" {
                    names.push(text);
                } else if current_tag == "NextMarker" {
                    next_marker = Some(text);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(BlobError::Other(format!("xml parse error: {e}")));
            }
            _ => {}
        }
    }

    Ok((names, next_marker))
}

// ── URL encoding ─────────────────────────────────────────────────────────────

/// Percent-encode a string for use as a URL path segment (object key) or as a
/// query parameter value (list `prefix`/`marker`).  `/` is left unescaped so
/// callers can use this to encode a full blob key while preserving its
/// path-segment structure.
///
/// Mirrors `oxistore_blob_s3::percent_encode` — see the S3 backend for the
/// equivalent fix (this crate's `blob_url` previously used keys verbatim,
/// which broke on `?`, `#`, spaces, and non-ASCII characters).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(hex_nibble(byte >> 4));
                out.push(hex_nibble(byte & 0x0f));
            }
        }
    }
    out
}

fn hex_nibble(n: u8) -> char {
    if n < 10 {
        (b'0' + n) as char
    } else {
        (b'A' + n - 10) as char
    }
}

// ── Clone for Client (needed for async closures) ─────────────────────────────
// `Client<HttpConnector>` implements Clone per oxihttp-client source.

//! Integration tests for `oxistore-blob-azure`.
//!
//! All tests use a minimal in-process mock HTTP/1.1 server so no real Azure
//! account is needed.  Tests validate the request shape (headers, verb, body)
//! and confirm the BlobStore impl interprets responses correctly.

use bytes::Bytes;
use oxistore_blob::{BlobError, BlobStore};
use oxistore_blob_azure::{AzureBlobStore, AzureConfig, AzureCredentials, SharedKeySigner};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ── Mock server helpers ───────────────────────────────────────────────────────

/// A captured HTTP request from the mock server.
#[derive(Debug, Clone)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Spawn a one-shot HTTP/1.1 mock server that responds with `response` and
/// returns the parsed request.
///
/// Returns (port, handle) — join the handle to get the `CapturedRequest`.
async fn spawn_mock(response: &'static str) -> (u16, tokio::task::JoinHandle<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();

        // Read until \r\n\r\n to get the headers.
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).await.unwrap();
            raw.extend_from_slice(&buf[..n]);
            if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if n == 0 {
                break;
            }
        }

        // Parse request line and headers.
        let header_section = std::str::from_utf8(&raw).unwrap_or("").to_string();
        let mut lines = header_section.split("\r\n");

        let request_line = lines.next().unwrap_or("").to_string();
        let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
        let method = parts.first().copied().unwrap_or("").to_string();
        let path = parts.get(1).copied().unwrap_or("").to_string();

        let mut headers = Vec::new();
        let mut content_length: usize = 0;
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some(colon) = line.find(':') {
                let key = line[..colon].trim().to_ascii_lowercase();
                let val = line[colon + 1..].trim().to_string();
                if key == "content-length" {
                    content_length = val.parse().unwrap_or(0);
                }
                headers.push((key, val));
            }
        }

        // Read body if Content-Length > 0.
        let mut body = Vec::new();
        if content_length > 0 {
            // The tail of `raw` after \r\n\r\n may contain partial body.
            let split_pos = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let already = &raw[split_pos + 4..];
            body.extend_from_slice(already);
            while body.len() < content_length {
                let n = stream.read(&mut buf).await.unwrap();
                body.extend_from_slice(&buf[..n]);
                if n == 0 {
                    break;
                }
            }
        }

        // Send response.
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();

        CapturedRequest {
            method,
            path,
            headers,
            body,
        }
    });

    (port, handle)
}

/// Spawn a mock that responds to two sequential requests (for pagination tests).
async fn spawn_mock_sequence(
    responses: Vec<&'static str>,
) -> (u16, tokio::task::JoinHandle<Vec<CapturedRequest>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        let mut captured = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();

            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                raw.extend_from_slice(&buf[..n]);
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if n == 0 {
                    break;
                }
            }

            let header_section = std::str::from_utf8(&raw).unwrap_or("").to_string();
            let mut lines = header_section.split("\r\n");
            let request_line = lines.next().unwrap_or("").to_string();
            let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
            let method = parts.first().copied().unwrap_or("").to_string();
            let path = parts.get(1).copied().unwrap_or("").to_string();

            let mut headers = Vec::new();
            let mut content_length: usize = 0;
            for line in lines {
                if line.is_empty() {
                    break;
                }
                if let Some(colon) = line.find(':') {
                    let key = line[..colon].trim().to_ascii_lowercase();
                    let val = line[colon + 1..].trim().to_string();
                    if key == "content-length" {
                        content_length = val.parse().unwrap_or(0);
                    }
                    headers.push((key, val));
                }
            }

            let mut body = Vec::new();
            if content_length > 0 {
                let split_pos = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
                let already = &raw[split_pos + 4..];
                body.extend_from_slice(already);
                let mut buf2 = [0u8; 4096];
                while body.len() < content_length {
                    let n = stream.read(&mut buf2).await.unwrap();
                    body.extend_from_slice(&buf2[..n]);
                    if n == 0 {
                        break;
                    }
                }
            }

            stream.write_all(response.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();

            captured.push(CapturedRequest {
                method,
                path,
                headers,
                body,
            });
        }
        captured
    });

    (port, handle)
}

/// Create a test AzureConfig pointing at a local mock server.
fn mock_config(port: u16) -> AzureConfig {
    // Use a 32-byte key encoded in base64 (all zeros — valid for tests).
    let key_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [0u8; 32]);
    let creds = AzureCredentials {
        account_name: "testaccount".to_string(),
        account_key_b64: key_b64,
    };
    let mut cfg = AzureConfig::new(creds, "testcontainer");
    cfg.endpoint = Some(format!("http://127.0.0.1:{port}"));
    cfg.timeout = std::time::Duration::from_secs(5);
    cfg
}

// ── Test: Connection string parsing ──────────────────────────────────────────

#[test]
fn azure_connection_string_parse_extracts_account_name_and_key() {
    let conn_str = "DefaultEndpointsProtocol=https;AccountName=myaccount;AccountKey=dGVzdGtleQ==;EndpointSuffix=core.windows.net";
    let creds = AzureCredentials::from_connection_string(conn_str).unwrap();
    assert_eq!(creds.account_name, "myaccount");
    assert_eq!(creds.account_key_b64, "dGVzdGtleQ==");
}

#[test]
fn azure_connection_string_missing_account_name_returns_err() {
    let conn_str = "AccountKey=somekey";
    assert!(AzureCredentials::from_connection_string(conn_str).is_err());
}

#[test]
fn azure_connection_string_missing_account_key_returns_err() {
    let conn_str = "AccountName=myaccount";
    assert!(AzureCredentials::from_connection_string(conn_str).is_err());
}

// ── Test: HMAC-SHA256 signature ───────────────────────────────────────────────

#[test]
fn azure_hmac_sha256_signature_non_empty() {
    let key = vec![0u8; 32];
    let signer = SharedKeySigner::new("testaccount".to_string(), key);
    let headers = &[
        ("x-ms-date", "Wed, 27 May 2026 00:00:00 GMT"),
        ("x-ms-version", "2024-08-04"),
        ("x-ms-blob-type", "BlockBlob"),
    ];
    let auth = signer
        .sign(
            "PUT",
            "http://127.0.0.1:12345/testcontainer/myblob",
            headers,
            Some(5),
            Some("application/octet-stream"),
        )
        .unwrap();

    // Signature should be non-empty and in "SharedKey <account>:<base64>" form.
    assert!(auth.starts_with("SharedKey testaccount:"));
    let sig_part = auth.trim_start_matches("SharedKey testaccount:");
    assert!(
        sig_part.len() >= 44,
        "base64 sig should be at least 44 chars, got {sig_part}"
    );
}

// ── Test: Authorization header format ────────────────────────────────────────

#[test]
fn azure_authorization_header_format() {
    let key = vec![1u8; 32];
    let signer = SharedKeySigner::new("account42".to_string(), key);
    let auth = signer
        .sign("GET", "http://host/container/blob", &[], None, None)
        .unwrap();
    assert!(auth.starts_with("SharedKey account42:"));
    let after = &auth["SharedKey account42:".len()..];
    // Should be valid base64.
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD.decode(after);
    assert!(
        decoded.is_ok(),
        "authorization signature is not valid base64"
    );
    assert_eq!(
        decoded.unwrap().len(),
        32,
        "HMAC-SHA256 output must be 32 bytes"
    );
}

// ── Test: PUT then GET round-trip ─────────────────────────────────────────────

#[tokio::test]
async fn azure_put_then_get_roundtrip() {
    // PUT mock: respond 201 Created
    let (put_port, put_handle) =
        spawn_mock("HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n").await;
    let store = AzureBlobStore::new(mock_config(put_port)).unwrap();
    store
        .put("hello.txt", Bytes::from("hello world"))
        .await
        .unwrap();
    let put_req = put_handle.await.unwrap();
    assert_eq!(put_req.method, "PUT");
    assert_eq!(put_req.body, b"hello world");

    // GET mock: respond 200 with the same body
    let (get_port, get_handle) =
        spawn_mock("HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world").await;
    let store2 = AzureBlobStore::new(mock_config(get_port)).unwrap();
    let data = store2.get("hello.txt").await.unwrap();
    assert_eq!(data.as_ref(), b"hello world");
    let get_req = get_handle.await.unwrap();
    assert_eq!(get_req.method, "GET");
}

// ── Test: GET missing blob returns NotFound ────────────────────────────────────

#[tokio::test]
async fn azure_get_missing_returns_not_found() {
    let (port, _handle) = spawn_mock("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
    let store = AzureBlobStore::new(mock_config(port)).unwrap();
    let err = store.get("missing.bin").await.unwrap_err();
    assert!(
        matches!(err, BlobError::NotFound(_)),
        "expected NotFound, got: {err:?}"
    );
}

// ── Test: HEAD returns size ───────────────────────────────────────────────────

#[tokio::test]
async fn azure_head_returns_size() {
    let (port, _handle) =
        spawn_mock("HTTP/1.1 200 OK\r\nContent-Length: 42\r\nContent-Type: image/png\r\n\r\n")
            .await;
    let store = AzureBlobStore::new(mock_config(port)).unwrap();
    let meta = store.head("photo.png").await.unwrap();
    assert_eq!(meta.size, 42);
    assert_eq!(meta.content_type.as_deref(), Some("image/png"));
    assert_eq!(meta.key, "photo.png");
}

// ── Test: HEAD missing blob returns NotFound ──────────────────────────────────

#[tokio::test]
async fn azure_head_missing_returns_not_found() {
    let (port, _handle) = spawn_mock("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
    let store = AzureBlobStore::new(mock_config(port)).unwrap();
    let err = store.head("ghost.dat").await.unwrap_err();
    assert!(matches!(err, BlobError::NotFound(_)));
}

// ── Test: DELETE sends DELETE request ────────────────────────────────────────

#[tokio::test]
async fn azure_delete_sends_delete_request() {
    let (port, handle) = spawn_mock("HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n").await;
    let store = AzureBlobStore::new(mock_config(port)).unwrap();
    store.delete("removeme.txt").await.unwrap();
    let req = handle.await.unwrap();
    assert_eq!(req.method, "DELETE");
    assert!(req.path.contains("removeme.txt"));
}

// ── Test: DELETE missing returns NotFound ─────────────────────────────────────

#[tokio::test]
async fn azure_delete_missing_returns_not_found() {
    let (port, _handle) = spawn_mock("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
    let store = AzureBlobStore::new(mock_config(port)).unwrap();
    let err = store.delete("nope.txt").await.unwrap_err();
    assert!(matches!(err, BlobError::NotFound(_)));
}

// ── Test: LIST pagination yields all keys ────────────────────────────────────

// Content-Length values verified by Python:
//   page1: 198 bytes
//   page2: 154 bytes
// Both responses carry Connection: close so the client doesn't reuse the TCP
// connection between the two mock server instances.

static LIST_PAGE_1: &str = concat!(
    "HTTP/1.1 200 OK\r\n",
    "Content-Type: application/xml\r\n",
    "Content-Length: 198\r\n",
    "Connection: close\r\n",
    "\r\n",
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
    "<EnumerationResults>",
    "<Blobs>",
    "<Blob><Name>alpha.bin</Name></Blob>",
    "<Blob><Name>beta.bin</Name></Blob>",
    "</Blobs>",
    "<NextMarker>PAGE2TOKEN</NextMarker>",
    "</EnumerationResults>"
);

static LIST_PAGE_2: &str = concat!(
    "HTTP/1.1 200 OK\r\n",
    "Content-Type: application/xml\r\n",
    "Content-Length: 154\r\n",
    "Connection: close\r\n",
    "\r\n",
    "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
    "<EnumerationResults>",
    "<Blobs>",
    "<Blob><Name>gamma.bin</Name></Blob>",
    "</Blobs>",
    "<NextMarker></NextMarker>",
    "</EnumerationResults>"
);

#[tokio::test]
async fn azure_list_pagination_yields_all_keys() {
    let (port, handle) = spawn_mock_sequence(vec![LIST_PAGE_1, LIST_PAGE_2]).await;
    let store = AzureBlobStore::new(mock_config(port)).unwrap();
    let keys = store.list("").await.unwrap();
    let _reqs = handle.await.unwrap();
    assert_eq!(keys.len(), 3, "should have 3 blobs across 2 pages");
    assert!(keys.contains(&"alpha.bin".to_string()));
    assert!(keys.contains(&"beta.bin".to_string()));
    assert!(keys.contains(&"gamma.bin".to_string()));
}

// ── Test: x-ms-date header present on PUT ────────────────────────────────────

#[tokio::test]
async fn azure_x_ms_date_header_present() {
    let (port, handle) = spawn_mock("HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n").await;
    let store = AzureBlobStore::new(mock_config(port)).unwrap();
    store
        .put("check-headers.bin", Bytes::from("data"))
        .await
        .unwrap();
    let req = handle.await.unwrap();
    let has_date = req.headers.iter().any(|(k, _)| k == "x-ms-date");
    assert!(
        has_date,
        "x-ms-date header must be present; headers={:?}",
        req.headers
    );
}

// ── Test: x-ms-version header present ────────────────────────────────────────

#[tokio::test]
async fn azure_x_ms_version_header_present() {
    let (port, handle) = spawn_mock("HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n").await;
    let store = AzureBlobStore::new(mock_config(port)).unwrap();
    store
        .put("versioned.bin", Bytes::from("payload"))
        .await
        .unwrap();
    let req = handle.await.unwrap();
    let version_hdr = req
        .headers
        .iter()
        .find(|(k, _)| k == "x-ms-version")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        version_hdr,
        Some("2024-08-04"),
        "x-ms-version must be 2024-08-04; headers={:?}",
        req.headers
    );
}

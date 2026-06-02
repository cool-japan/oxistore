//! Integration tests for `oxistore-blob-gcs` using an in-process mock HTTP server.
//!
//! A [`MockServer`] binds a `tokio::net::TcpListener` on a random port and
//! services plain HTTP/1.1 requests, allowing tests to run without a real GCS
//! connection or external credentials.
//!
//! The service account JSON (including RSA private key) is generated once at
//! test startup using `oxicrypto_sig::rsa_generate_keypair` and written to a
//! temporary directory, keeping no key material committed to the repository.

#![allow(clippy::needless_return)]

use bytes::Bytes;
use oxicrypto_sig::rsa_generate_keypair;
use oxistore_blob::BlobStore;
use oxistore_blob_gcs::{GcsBlobStore, GcsConfig, GcsServiceAccount};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ── Test RSA key (generated once for the whole binary) ────────────────────────

/// Returns `(pkcs8_der, pem_string)` for the test private key.
fn test_key() -> &'static (Vec<u8>, String) {
    static KEY: OnceLock<(Vec<u8>, String)> = OnceLock::new();
    KEY.get_or_init(|| {
        let (der, _pub_der) = rsa_generate_keypair(2048).expect("rsa_generate_keypair(2048)");
        let b64 = base64_std(&der);
        // Wrap in 64-char lines (standard PEM format)
        let wrapped: String = b64
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).expect("base64 is ASCII"))
            .collect::<Vec<_>>()
            .join("\n");
        let pem = format!("-----BEGIN PRIVATE KEY-----\n{wrapped}\n-----END PRIVATE KEY-----\n");
        (der, pem)
    })
}

/// Build a service account JSON string with the test key.
fn test_sa_json(token_uri: &str) -> String {
    let (_, pem) = test_key();
    // Escape newlines for JSON
    let pem_escaped = pem.replace('\n', "\\n");
    format!(
        r#"{{
  "type": "service_account",
  "project_id": "test-project",
  "client_email": "test@test-project.iam.gserviceaccount.com",
  "private_key": "{pem_escaped}",
  "token_uri": "{token_uri}"
}}"#
    )
}

/// Standard (padded) base64 encoding.
fn base64_std(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

// ── Minimal mock HTTP/1.1 server ──────────────────────────────────────────────

/// A request seen by the mock server.
#[derive(Debug, Clone)]
struct MockRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

type SharedLog = Arc<Mutex<Vec<MockRequest>>>;

/// Bind a random-port listener and return (port, log, shutdown sender).
///
/// The mock server handles:
/// - `POST /token` — OAuth2 token endpoint
/// - `POST /upload/...` — GCS simple upload
/// - `GET /storage/v1/b/.../o/<key>?alt=media` — download
/// - `GET /storage/v1/b/.../o/<key>` (no alt=media) — metadata
/// - `DELETE /storage/v1/b/.../o/<key>` — delete
/// - `GET /storage/v1/b/.../o?...` — list
async fn start_mock_server() -> (u16, SharedLog) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let log: SharedLog = Arc::new(Mutex::new(Vec::new()));
    let log_clone = Arc::clone(&log);

    tokio::spawn(async move {
        // We use a simple "objects" map as the store state.
        let objects: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));

        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let log2 = Arc::clone(&log_clone);
            let objects2 = Arc::clone(&objects);

            tokio::spawn(async move {
                // Read the full request (naively, reading until EOF works for Connection: close)
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            raw.extend_from_slice(&buf[..n]);
                            // Check if we have the full request (header + body based on Content-Length)
                            if let Some(hdr_end) = find_crlf2(&raw) {
                                let hdr_section =
                                    std::str::from_utf8(&raw[..hdr_end]).unwrap_or("");
                                let content_length: usize = hdr_section
                                    .lines()
                                    .skip(1)
                                    .find_map(|l| {
                                        let lower = l.to_lowercase();
                                        lower
                                            .strip_prefix("content-length:")
                                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                                    })
                                    .unwrap_or(0);
                                let body_start = hdr_end + 4;
                                if raw.len() >= body_start + content_length {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }

                let req = parse_http_request(&raw);
                log2.lock().unwrap().push(req.clone());

                let response = handle_mock_request(&req, &objects2);
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    (port, log)
}

fn find_crlf2(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_http_request(raw: &[u8]) -> MockRequest {
    let hdr_end = find_crlf2(raw).unwrap_or(raw.len());
    let hdr_str = std::str::from_utf8(&raw[..hdr_end]).unwrap_or("");
    let mut lines = hdr_str.lines();
    let request_line = lines.next().unwrap_or("");
    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    let method = parts.first().copied().unwrap_or("").to_string();
    let path = parts.get(1).copied().unwrap_or("").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }

    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let body_start = hdr_end + 4;
    let body = if body_start + content_length <= raw.len() {
        raw[body_start..body_start + content_length].to_vec()
    } else if body_start <= raw.len() {
        raw[body_start..].to_vec()
    } else {
        Vec::new()
    };

    MockRequest {
        method,
        path,
        headers,
        body,
    }
}

fn handle_mock_request(
    req: &MockRequest,
    objects: &Arc<Mutex<HashMap<String, Vec<u8>>>>,
) -> String {
    // Token endpoint
    if req.path == "/token" && req.method == "POST" {
        let body = r#"{"access_token":"test_token_abc","token_type":"Bearer","expires_in":3600}"#;
        return http_response(200, "application/json", body.as_bytes());
    }

    // GCS upload: POST /upload/storage/v1/b/<bucket>/o?uploadType=media&name=<key>
    if req.method == "POST" && req.path.contains("/upload/storage/v1/b/") {
        let key = extract_query_param(&req.path, "name").unwrap_or_default();
        objects
            .lock()
            .unwrap()
            .insert(key.clone(), req.body.clone());
        let resp_body = format!(
            r#"{{"kind":"storage#object","name":"{key}","bucket":"test-bucket","size":"{}"}}"#,
            req.body.len()
        );
        return http_response(200, "application/json", resp_body.as_bytes());
    }

    // GCS list: GET /storage/v1/b/<bucket>/o?...
    if req.method == "GET" && req.path.contains("/storage/v1/b/") && !path_has_object_key(&req.path)
    {
        let prefix = extract_query_param(&req.path, "prefix").unwrap_or_default();
        let page_token = extract_query_param(&req.path, "pageToken");

        let objs = objects.lock().unwrap();
        let all_keys: Vec<&String> = objs.keys().filter(|k| k.starts_with(&prefix)).collect();

        // Simulate pagination: first page returns 2 items + nextPageToken if >2
        let page_size = 2usize;
        let start = match page_token.as_deref() {
            Some("page2") => page_size,
            _ => 0,
        };
        let page: Vec<&String> = all_keys
            .iter()
            .skip(start)
            .take(page_size)
            .copied()
            .collect();
        let has_more = all_keys.len() > start + page_size;

        let items_json: Vec<String> = page
            .iter()
            .map(|k| format!(r#"{{"name":"{k}","size":"1"}}"#))
            .collect();
        let items_str = items_json.join(",");

        let mut resp_body = format!(r#"{{"kind":"storage#objects","items":[{items_str}]}}"#);
        if has_more {
            resp_body = format!(
                r#"{{"kind":"storage#objects","items":[{items_str}],"nextPageToken":"page2"}}"#
            );
        }
        return http_response(200, "application/json", resp_body.as_bytes());
    }

    // Parse GCS object path: /storage/v1/b/<bucket>/o/<key>
    if let Some(key) = extract_object_key(&req.path) {
        let has_alt_media = req.path.contains("alt=media");

        match req.method.as_str() {
            "GET" if has_alt_media => {
                // Download
                let objs = objects.lock().unwrap();
                if let Some(data) = objs.get(&key) {
                    return http_response(200, "application/octet-stream", data);
                } else {
                    let err = r#"{"error":{"code":404,"message":"Not Found"}}"#;
                    return http_response(404, "application/json", err.as_bytes());
                }
            }
            "GET" => {
                // Metadata
                let objs = objects.lock().unwrap();
                if let Some(data) = objs.get(&key) {
                    let meta = format!(
                        r#"{{"kind":"storage#object","name":"{key}","size":"{}","contentType":"application/octet-stream"}}"#,
                        data.len()
                    );
                    return http_response(200, "application/json", meta.as_bytes());
                } else {
                    let err = r#"{"error":{"code":404,"message":"Not Found"}}"#;
                    return http_response(404, "application/json", err.as_bytes());
                }
            }
            "DELETE" => {
                let existed = objects.lock().unwrap().remove(&key).is_some();
                if existed {
                    return http_response_empty(204);
                } else {
                    let err = r#"{"error":{"code":404,"message":"Not Found"}}"#;
                    return http_response(404, "application/json", err.as_bytes());
                }
            }
            _ => {}
        }
    }

    http_response(404, "application/json", b"{\"error\":{\"code\":404}}")
}

fn http_response(status: u16, content_type: &str, body: &[u8]) -> String {
    format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ) + std::str::from_utf8(body).unwrap_or("")
}

fn http_response_empty(status: u16) -> String {
    format!("HTTP/1.1 {status} No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

/// Extract the object key from a GCS path like `/storage/v1/b/bucket/o/key`.
fn extract_object_key(path: &str) -> Option<String> {
    // Strip query string
    let path_only = path.split('?').next().unwrap_or(path);
    // Pattern: /storage/v1/b/<bucket>/o/<key>
    let prefix = "/storage/v1/b/";
    let after = path_only.strip_prefix(prefix)?;
    // Find `/o/`
    let o_pos = after.find("/o/")?;
    let key_encoded = &after[o_pos + 3..];
    // URL-decode (simple percent decode)
    Some(percent_decode(key_encoded))
}

/// Returns true when the path has a specific object key (as opposed to listing).
fn path_has_object_key(path: &str) -> bool {
    let path_only = path.split('?').next().unwrap_or(path);
    let re = "/storage/v1/b/";
    if let Some(after) = path_only.strip_prefix(re) {
        if let Some(o_pos) = after.find("/o/") {
            let key = &after[o_pos + 3..];
            return !key.is_empty();
        }
    }
    false
}

fn extract_query_param(url: &str, param: &str) -> Option<String> {
    let query = url.split('?').nth(1)?;
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            if k == param {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

/// Minimal percent-decode (handles `%XX` sequences).
fn percent_decode(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.bytes().peekable();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            if let Ok(n) = u8::from_str_radix(std::str::from_utf8(&[hi, lo]).unwrap_or("00"), 16) {
                out.push(n as char);
            }
        } else if b == b'+' {
            out.push(' ');
        } else {
            out.push(b as char);
        }
    }
    out
}

// ── Test helper: write service account JSON to a temp file ────────────────────

fn write_sa_file(token_uri: &str) -> (tempfile_path::TempPath, String) {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_sa_{}.json", std::process::id()));
    let json = test_sa_json(token_uri);
    std::fs::write(&path, &json).expect("write SA JSON");
    (tempfile_path::TempPath(path), json)
}

/// RAII guard that removes a file on drop.
mod tempfile_path {
    pub struct TempPath(pub std::path::PathBuf);
    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// Build a `GcsConfig` pointing at a running mock server.
fn mock_config(port: u16, sa: GcsServiceAccount) -> GcsConfig {
    let base = format!("http://127.0.0.1:{port}");
    GcsConfig {
        bucket: "test-bucket".to_string(),
        credentials: sa,
        timeout: Duration::from_secs(5),
        endpoint: Some(base.clone()),
        oauth_endpoint: Some(format!("{base}/token")),
    }
}

fn parse_sa_from_json(json: &str) -> GcsServiceAccount {
    GcsServiceAccount::from_json_str(json).expect("parse SA JSON")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

/// 1. `GcsServiceAccount::from_json_file` parses fields correctly.
#[tokio::test]
async fn gcs_service_account_from_json_file_parses_fields() {
    let token_uri = "https://oauth2.googleapis.com/token";
    let (guard, _json) = write_sa_file(token_uri);
    let sa = GcsServiceAccount::from_json_file(&guard.0).expect("from_json_file");
    assert_eq!(sa.client_email, "test@test-project.iam.gserviceaccount.com");
    assert_eq!(sa.project_id.as_deref(), Some("test-project"));
    assert!(sa.private_key_pem.contains("-----BEGIN PRIVATE KEY-----"));
}

/// 2. Built JWT header contains `alg:RS256` and `typ:JWT`.
#[tokio::test]
async fn gcs_jwt_is_rfc7519_compliant() {
    use base64::Engine;
    let (port, _log) = start_mock_server().await;
    let token_uri = format!("http://127.0.0.1:{port}/token");
    let json = test_sa_json(&token_uri);
    let sa = parse_sa_from_json(&json);
    let jwt = oxistore_blob_gcs::auth::build_jwt(&sa, &token_uri).expect("build_jwt");
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3, "JWT must have 3 parts");
    let header_json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[0])
        .expect("base64 decode header");
    let header: serde_json::Value =
        serde_json::from_slice(&header_json).expect("parse header JSON");
    assert_eq!(header["alg"], "RS256");
    assert_eq!(header["typ"], "JWT");
}

/// 3. Token exchange returns the bearer token from the mock.
#[tokio::test]
async fn gcs_token_exchange_returns_bearer() {
    let (port, _log) = start_mock_server().await;
    let token_uri = format!("http://127.0.0.1:{port}/token");
    let json = test_sa_json(&token_uri);
    let sa = parse_sa_from_json(&json);
    let config = mock_config(port, sa);
    let store = GcsBlobStore::new(config).expect("new store");
    // Trigger token acquisition by doing a list (which goes to mock server)
    let keys = store.list("").await.expect("list");
    drop(keys); // just verifying it worked without error
}

/// 4. `put` then `get` round-trips bytes correctly.
#[tokio::test]
async fn gcs_put_then_get_roundtrip() {
    let (port, _log) = start_mock_server().await;
    let token_uri = format!("http://127.0.0.1:{port}/token");
    let json = test_sa_json(&token_uri);
    let sa = parse_sa_from_json(&json);
    let config = mock_config(port, sa);
    let store = GcsBlobStore::new(config).expect("new store");

    let data = Bytes::from("hello gcs roundtrip");
    store
        .put("test/roundtrip.txt", data.clone())
        .await
        .expect("put");
    let got = store.get("test/roundtrip.txt").await.expect("get");
    assert_eq!(got, data);
}

/// 5. `get` on a missing key returns `BlobError::NotFound`.
#[tokio::test]
async fn gcs_get_missing_returns_not_found() {
    let (port, _log) = start_mock_server().await;
    let token_uri = format!("http://127.0.0.1:{port}/token");
    let json = test_sa_json(&token_uri);
    let sa = parse_sa_from_json(&json);
    let config = mock_config(port, sa);
    let store = GcsBlobStore::new(config).expect("new store");

    let result = store.get("does/not/exist.txt").await;
    assert!(
        matches!(result, Err(oxistore_blob::BlobError::NotFound(_))),
        "expected NotFound, got: {result:?}"
    );
}

/// 6. `head` returns correct `BlobMeta` with size.
#[tokio::test]
async fn gcs_head_returns_meta() {
    let (port, _log) = start_mock_server().await;
    let token_uri = format!("http://127.0.0.1:{port}/token");
    let json = test_sa_json(&token_uri);
    let sa = parse_sa_from_json(&json);
    let config = mock_config(port, sa);
    let store = GcsBlobStore::new(config).expect("new store");

    let data = Bytes::from("hello meta check");
    store.put("meta/test.bin", data.clone()).await.expect("put");
    let meta = store.head("meta/test.bin").await.expect("head");
    assert_eq!(meta.size, data.len() as u64);
    assert_eq!(meta.key, "meta/test.bin");
}

/// 7. `delete` removes the object; subsequent get returns `NotFound`.
#[tokio::test]
async fn gcs_delete_removes_object() {
    let (port, log) = start_mock_server().await;
    let token_uri = format!("http://127.0.0.1:{port}/token");
    let json = test_sa_json(&token_uri);
    let sa = parse_sa_from_json(&json);
    let config = mock_config(port, sa);
    let store = GcsBlobStore::new(config).expect("new store");

    store
        .put("delete/me.txt", Bytes::from("bye"))
        .await
        .expect("put");
    store.delete("delete/me.txt").await.expect("delete");

    // Verify a DELETE request was sent (drop lock before further store calls)
    {
        let seen = log.lock().unwrap();
        let delete_req = seen.iter().any(|r| r.method == "DELETE");
        assert!(delete_req, "expected a DELETE request in the log");
    }

    // Object should now be gone
    let result = store.get("delete/me.txt").await;
    assert!(
        matches!(result, Err(oxistore_blob::BlobError::NotFound(_))),
        "expected NotFound after delete, got: {result:?}"
    );
}

/// 8. `list` follows `nextPageToken` pagination and returns all keys.
#[tokio::test]
async fn gcs_list_pagination_yields_all_keys() {
    let (port, _log) = start_mock_server().await;
    let token_uri = format!("http://127.0.0.1:{port}/token");
    let json = test_sa_json(&token_uri);
    let sa = parse_sa_from_json(&json);
    let config = mock_config(port, sa);
    let store = GcsBlobStore::new(config).expect("new store");

    // Insert 3 objects so that mock pagination kicks in (page_size=2)
    store
        .put("page/a.txt", Bytes::from("a"))
        .await
        .expect("put a");
    store
        .put("page/b.txt", Bytes::from("b"))
        .await
        .expect("put b");
    store
        .put("page/c.txt", Bytes::from("c"))
        .await
        .expect("put c");

    let keys = store.list("page/").await.expect("list");
    // All 3 should be returned across the two pages
    assert_eq!(keys.len(), 3, "expected 3 keys, got: {keys:?}");
    assert!(keys.contains(&"page/a.txt".to_string()));
    assert!(keys.contains(&"page/b.txt".to_string()));
    assert!(keys.contains(&"page/c.txt".to_string()));
}

/// 9. Every non-token request carries `Authorization: Bearer test_token_abc`.
#[tokio::test]
async fn gcs_authorization_header_present() {
    let (port, log) = start_mock_server().await;
    let token_uri = format!("http://127.0.0.1:{port}/token");
    let json = test_sa_json(&token_uri);
    let sa = parse_sa_from_json(&json);
    let config = mock_config(port, sa);
    let store = GcsBlobStore::new(config).expect("new store");

    store
        .put("auth/check.txt", Bytes::from("auth"))
        .await
        .expect("put");
    let _ = store.get("auth/check.txt").await.expect("get");

    let seen = log.lock().unwrap();
    let gcs_requests: Vec<&MockRequest> = seen.iter().filter(|r| r.path != "/token").collect();
    assert!(
        !gcs_requests.is_empty(),
        "expected at least one GCS request"
    );
    for req in &gcs_requests {
        let auth = req
            .headers
            .get("authorization")
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            auth, "Bearer test_token_abc",
            "request {} {} missing correct Bearer token",
            req.method, req.path
        );
    }
}

/// 10. `head` on a missing key returns `BlobError::NotFound`.
#[tokio::test]
async fn gcs_head_missing_returns_not_found() {
    let (port, _log) = start_mock_server().await;
    let token_uri = format!("http://127.0.0.1:{port}/token");
    let json = test_sa_json(&token_uri);
    let sa = parse_sa_from_json(&json);
    let config = mock_config(port, sa);
    let store = GcsBlobStore::new(config).expect("new store");

    let result = store.head("missing/key.txt").await;
    assert!(
        matches!(result, Err(oxistore_blob::BlobError::NotFound(_))),
        "expected NotFound, got: {result:?}"
    );
}

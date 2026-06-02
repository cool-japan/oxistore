//! Integration tests for oxistore-blob-s3 using a minimal in-process mock server.
//!
//! The mock server binds to `127.0.0.1:0` (random port), responds to
//! HEAD/GET/PUT/DELETE with pre-canned HTTP/1.1 responses, and captures
//! request headers for auth validation.
//!
//! No external process, no wiremock, no hyper — just tokio TCP.

use bytes::Bytes;
use oxistore_blob::BlobStore;
use oxistore_blob_s3::{retry::RetryConfig, S3BlobStore, S3BlobStoreBuilder, S3Credentials};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ── Mock server helpers ───────────────────────────────────────────────────────

/// A canned HTTP response sent by the mock for a single request.
#[derive(Clone, Debug)]
struct MockResponse {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl MockResponse {
    fn ok_empty() -> Self {
        Self {
            status: 200,
            headers: vec![("content-length", "0".to_string())],
            body: vec![],
        }
    }

    fn ok_with_body(body: impl Into<Vec<u8>>) -> Self {
        let body = body.into();
        let len = body.len().to_string();
        Self {
            status: 200,
            headers: vec![("content-length", len)],
            body,
        }
    }

    fn ok_head(content_length: u64, content_type: &str) -> Self {
        Self {
            status: 200,
            headers: vec![
                ("content-length", content_length.to_string()),
                ("content-type", content_type.to_string()),
            ],
            body: vec![],
        }
    }

    fn not_found_xml() -> Self {
        let xml = b"<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>";
        let len = xml.len().to_string();
        Self {
            status: 404,
            headers: vec![
                ("content-type", "application/xml".to_string()),
                ("content-length", len),
            ],
            body: xml.to_vec(),
        }
    }

    fn no_content() -> Self {
        Self {
            status: 204,
            headers: vec![("content-length", "0".to_string())],
            body: vec![],
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {} OK\r\n", self.status).into_bytes();
        for (k, v) in &self.headers {
            out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        out.extend_from_slice(b"Connection: close\r\n");
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

/// Captured request for auth-header inspection.
#[derive(Debug, Default, Clone)]
struct CapturedRequest {
    headers: HashMap<String, String>,
}

/// Spawn a mock server that sends `responses` in order, one per accepted
/// connection.  Captures the request into `capture`.
///
/// Returns the port the server is listening on and a join handle.
///
/// The server reads bytes until the HTTP header terminator (`\r\n\r\n`) is
/// seen before responding, and explicitly shuts down the write side after
/// flushing so the client receives a clean EOF.  This prevents the
/// "Connection reset by peer" race that arises when the server task exits
/// before the client finishes reading under parallel test load.
async fn spawn_mock(
    responses: Vec<MockResponse>,
    capture: Arc<Mutex<Vec<CapturedRequest>>>,
) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let port = listener.local_addr().expect("local_addr").port();

    tokio::spawn(async move {
        for resp in responses {
            match listener.accept().await {
                Ok((mut conn, _)) => {
                    // Read until we see the end-of-headers marker (\r\n\r\n).
                    // A single read() is not guaranteed to deliver the whole
                    // request under parallel test load.
                    let mut buf = Vec::with_capacity(8192);
                    let mut tmp = [0u8; 4096];
                    loop {
                        match conn.read(&mut tmp).await {
                            Ok(0) => break, // EOF before complete headers
                            Ok(n) => {
                                buf.extend_from_slice(&tmp[..n]);
                                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }

                    // Consume the request body (if any) so the kernel sends
                    // FIN instead of RST on socket close.  RST occurs when the
                    // socket is closed with unread bytes in the receive buffer,
                    // which causes the client's `read_to_end` to fail with
                    // "Connection reset by peer (os error 54)".
                    let header_text = String::from_utf8_lossy(&buf);
                    let content_length: usize = header_text
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                v.trim().parse().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    let header_end = buf
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|p| p + 4)
                        .unwrap_or(buf.len());
                    let body_already = buf.len().saturating_sub(header_end);
                    let mut remaining = content_length.saturating_sub(body_already);
                    while remaining > 0 {
                        match conn.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => remaining = remaining.saturating_sub(n),
                        }
                    }

                    // Parse method/path/headers
                    let captured = parse_captured_request(&buf);

                    {
                        let mut guard = capture.lock().expect("lock");
                        guard.push(captured);
                    }

                    // Send canned response then shut down write side so the
                    // client sees EOF rather than a TCP reset.
                    let _ = conn.write_all(&resp.into_bytes()).await;
                    let _ = conn.flush().await;
                    let _ = conn.shutdown().await;
                }
                Err(_) => break,
            }
        }
    });

    port
}

fn parse_captured_request(raw: &[u8]) -> CapturedRequest {
    let text = String::from_utf8_lossy(raw);
    let mut lines = text.lines();
    // Skip the request line (method + path + version)
    let _ = lines.next();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }

    CapturedRequest { headers }
}

/// Build an S3BlobStore (trait object) pointing at a mock server on localhost.
fn mock_store(port: u16) -> impl BlobStore {
    mock_s3_store(port)
}

/// Build a concrete S3BlobStore pointing at a mock server on localhost.
fn mock_s3_store(port: u16) -> S3BlobStore {
    S3BlobStoreBuilder::new()
        .endpoint(format!("http://127.0.0.1:{port}"))
        .region("us-east-1")
        .bucket("testbucket")
        .credentials(S3Credentials::new("AKIATEST", "SECRETTEST", None))
        .path_style(true)
        .timeout_secs(5)
        .build()
        .expect("build store")
}

/// Build a mock S3BlobStore with retry config.
fn mock_s3_store_with_retry(port: u16, retry: RetryConfig) -> S3BlobStore {
    S3BlobStoreBuilder::new()
        .endpoint(format!("http://127.0.0.1:{port}"))
        .region("us-east-1")
        .bucket("testbucket")
        .credentials(S3Credentials::new("AKIATEST", "SECRETTEST", None))
        .path_style(true)
        .timeout_secs(5)
        .retry_config(retry)
        .build()
        .expect("build store")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// PUT a small body, GET it back via two separate mock connections.
#[tokio::test]
async fn s3_put_then_get_roundtrip() {
    let capture = Arc::new(Mutex::new(Vec::new()));

    // PUT → 200, GET → 200 with body
    let responses = vec![
        MockResponse::ok_empty(),
        MockResponse::ok_with_body(b"hello world".as_slice()),
    ];
    let port = spawn_mock(responses, capture.clone()).await;
    // give mock a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let store = mock_store(port);
    store
        .put("mykey", Bytes::from("hello world"))
        .await
        .expect("put");

    let data = store.get("mykey").await.expect("get");
    assert_eq!(data.as_ref(), b"hello world");
}

/// HEAD returns `BlobMeta` with the content-length from the mock.
#[tokio::test]
async fn s3_head_existing_returns_size() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let responses = vec![MockResponse::ok_head(42, "application/octet-stream")];
    let port = spawn_mock(responses, capture.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let store = mock_store(port);
    let meta = store.head("somefile").await.expect("head");
    assert_eq!(meta.size, 42);
    assert_eq!(meta.key, "somefile");
}

/// HEAD on a missing key → `Ok(None)` via `BlobStore::exists`.
#[tokio::test]
async fn s3_head_missing_returns_none() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let responses = vec![MockResponse::not_found_xml()];
    let port = spawn_mock(responses, capture.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let store = mock_store(port);
    let exists = store.exists("nosuchkey").await.expect("exists");
    assert!(!exists);
}

/// GET on a missing key → `Err(BlobError::NotFound)`.
#[tokio::test]
async fn s3_get_missing_returns_not_found_error() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let responses = vec![MockResponse::not_found_xml()];
    let port = spawn_mock(responses, capture.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let store = mock_store(port);
    let err = store
        .get("nosuchkey")
        .await
        .expect_err("should be NotFound");
    match err {
        oxistore_blob::BlobError::NotFound(k) => assert_eq!(k, "nosuchkey"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// DELETE succeeds (204), subsequent HEAD → NotFound (404).
#[tokio::test]
async fn s3_delete_then_head_is_none() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let responses = vec![
        MockResponse::no_content(),    // DELETE → 204
        MockResponse::not_found_xml(), // HEAD → 404
    ];
    let port = spawn_mock(responses, capture.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let store = mock_store(port);
    store.delete("delkey").await.expect("delete");
    let exists = store.exists("delkey").await.expect("exists");
    assert!(!exists);
}

/// The `Authorization` header must contain `AWS4-HMAC-SHA256`.
#[tokio::test]
async fn s3_auth_header_well_formed() {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let responses = vec![MockResponse::ok_empty()];
    let port = spawn_mock(responses, capture.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let store = mock_store(port);
    store
        .put("authtest", Bytes::from("data"))
        .await
        .expect("put");

    let requests = capture.lock().expect("lock");
    let req = requests.first().expect("at least one request captured");
    let auth = req
        .headers
        .get("authorization")
        .expect("authorization header present");
    assert!(
        auth.contains("AWS4-HMAC-SHA256"),
        "expected AWS4-HMAC-SHA256 in Authorization but got: {auth}"
    );
}

/// `S3Credentials::from_env_with` with a getter that returns `None` → `Err`.
#[tokio::test]
async fn s3_from_env_missing_credentials_errors() {
    let result = S3Credentials::from_env_with(|_key| None);
    assert!(
        result.is_err(),
        "expected Err when env vars are missing, got Ok"
    );
    let msg = result.expect_err("error").to_string();
    assert!(
        msg.contains("AWS_ACCESS_KEY_ID"),
        "error message should mention AWS_ACCESS_KEY_ID: {msg}"
    );
}

// ── New feature tests ─────────────────────────────────────────────────────────

/// Helper MockResponse variants for multipart / copy / pagination tests.
impl MockResponse {
    fn xml_body(status: u16, xml: impl Into<Vec<u8>>) -> Self {
        let body = xml.into();
        let len = body.len().to_string();
        Self {
            status,
            headers: vec![
                ("content-type", "application/xml".to_string()),
                ("content-length", len),
            ],
            body,
        }
    }

    fn with_etag(etag: &str) -> Self {
        let etag = etag.to_string();
        Self {
            status: 200,
            headers: vec![("content-length", "0".to_string()), ("etag", etag)],
            body: vec![],
        }
    }

    fn error(status: u16) -> Self {
        let body =
            b"<Error><Code>ServiceUnavailable</Code><Message>Try again</Message></Error>".to_vec();
        let len = body.len().to_string();
        Self {
            status,
            headers: vec![
                ("content-type", "application/xml".to_string()),
                ("content-length", len),
            ],
            body,
        }
    }
}

// -- Helper: read entire body from a TCP connection -------

async fn read_full_request(conn: &mut tokio::net::TcpStream) -> (Vec<u8>, Vec<u8>) {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 4096];
    loop {
        match conn.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    // Read body if Content-Length present
    let header_text = String::from_utf8_lossy(&buf).to_string();
    let content_length: usize = header_text
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(buf.len());
    let already = buf[header_end..].len();
    let mut body = buf[header_end..].to_vec();
    let mut remaining = content_length.saturating_sub(already);
    while remaining > 0 {
        match conn.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                body.extend_from_slice(&tmp[..n]);
                remaining = remaining.saturating_sub(n);
            }
        }
    }
    (buf[..header_end].to_vec(), body)
}

/// Multipart: create_upload + 3 parts + complete → server receives CompleteMultipartUpload.
#[tokio::test]
async fn s3_multipart_3part_complete() {
    use tokio::io::AsyncWriteExt;
    // We capture the body of every request; the 5th one (index 4) is the CompleteMultipartUpload.
    let captured_bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_bodies2 = captured_bodies.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();

    // We need 5 requests: create(POST), part1(PUT), part2(PUT), part3(PUT), complete(POST)
    let responses: Vec<MockResponse> = vec![
        MockResponse::xml_body(
            200,
            b"<?xml version=\"1.0\"?><InitiateMultipartUploadResult><Bucket>testbucket</Bucket><Key>bigfile</Key><UploadId>upload123</UploadId></InitiateMultipartUploadResult>".as_slice(),
        ),
        MockResponse::with_etag("\"etag1\""),
        MockResponse::with_etag("\"etag2\""),
        MockResponse::with_etag("\"etag3\""),
        MockResponse::xml_body(
            200,
            b"<?xml version=\"1.0\"?><CompleteMultipartUploadResult><Location>http://testbucket.s3.amazonaws.com/bigfile</Location></CompleteMultipartUploadResult>".as_slice(),
        ),
    ];

    tokio::spawn(async move {
        for resp in responses {
            if let Ok((mut conn, _)) = listener.accept().await {
                let (_headers, body) = read_full_request(&mut conn).await;
                {
                    let mut guard = captured_bodies2.lock().expect("lock");
                    guard.push(body);
                }
                let _ = conn.write_all(&resp.into_bytes()).await;
                let _ = conn.flush().await;
                let _ = conn.shutdown().await;
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let store = mock_s3_store(port);
    let mut upload = store
        .create_multipart_upload("bigfile")
        .await
        .expect("create_multipart_upload");

    upload
        .upload_part(1, Bytes::from(vec![0u8; 100]))
        .await
        .expect("part 1");
    upload
        .upload_part(2, Bytes::from(vec![1u8; 100]))
        .await
        .expect("part 2");
    upload
        .upload_part(3, Bytes::from(vec![2u8; 100]))
        .await
        .expect("part 3");
    upload.complete().await.expect("complete");

    // Request index 4 (the 5th) is the CompleteMultipartUpload POST
    let bodies = captured_bodies.lock().expect("lock");
    assert_eq!(bodies.len(), 5, "expected 5 requests, got {}", bodies.len());
    let complete_body = &bodies[4];
    let xml = String::from_utf8_lossy(complete_body);
    assert!(
        xml.contains("<CompleteMultipartUpload>"),
        "complete XML tag missing: {xml}"
    );
    assert!(
        xml.contains("<PartNumber>1</PartNumber>"),
        "part 1 in xml: {xml}"
    );
    assert!(
        xml.contains("<PartNumber>2</PartNumber>"),
        "part 2 in xml: {xml}"
    );
    assert!(
        xml.contains("<PartNumber>3</PartNumber>"),
        "part 3 in xml: {xml}"
    );
}

/// Multipart abort: create + upload 1 part + abort → DELETE sent.
#[tokio::test]
async fn s3_multipart_abort() {
    use tokio::io::AsyncWriteExt;
    let captured_methods: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_methods2 = captured_methods.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let responses = vec![
        MockResponse::xml_body(
            200,
            b"<?xml version=\"1.0\"?><InitiateMultipartUploadResult><Bucket>testbucket</Bucket><Key>f</Key><UploadId>uid42</UploadId></InitiateMultipartUploadResult>".as_slice(),
        ),
        MockResponse::with_etag("\"etag1\""),
        // Abort response (204)
        MockResponse::no_content(),
    ];

    tokio::spawn(async move {
        for resp in responses {
            if let Ok((mut conn, _)) = listener.accept().await {
                let (headers, _body) = read_full_request(&mut conn).await;
                let first_line = String::from_utf8_lossy(&headers)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                {
                    let mut guard = captured_methods2.lock().expect("lock");
                    guard.push(first_line);
                }
                let _ = conn.write_all(&resp.into_bytes()).await;
                let _ = conn.flush().await;
                let _ = conn.shutdown().await;
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let store = mock_s3_store(port);
    let mut upload = store.create_multipart_upload("f").await.expect("create");
    upload
        .upload_part(1, Bytes::from("data"))
        .await
        .expect("part 1");
    upload.abort().await.expect("abort");

    let methods = captured_methods.lock().expect("lock");
    // The last request should be a DELETE (abort)
    let last = methods.last().expect("at least one request");
    assert!(
        last.starts_with("DELETE"),
        "expected DELETE for abort, got: {last}"
    );
}

/// Multipart: parts uploaded out of order must appear sorted in complete XML.
#[tokio::test]
async fn s3_multipart_parts_sorted_in_complete() {
    use tokio::io::AsyncWriteExt;
    let captured_complete_xml: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_clone = captured_complete_xml.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();

    // Responses for: create, part2, part1, part3, complete
    let responses = vec![
        MockResponse::xml_body(
            200,
            b"<?xml version=\"1.0\"?><InitiateMultipartUploadResult><Bucket>testbucket</Bucket><Key>f</Key><UploadId>uidXX</UploadId></InitiateMultipartUploadResult>".as_slice(),
        ),
        MockResponse::with_etag("\"etagB\""), // part 2
        MockResponse::with_etag("\"etagA\""), // part 1
        MockResponse::with_etag("\"etagC\""), // part 3
        MockResponse::xml_body(
            200,
            b"<?xml version=\"1.0\"?><CompleteMultipartUploadResult/>".as_slice(),
        ),
    ];

    tokio::spawn(async move {
        let mut req_index = 0;
        for resp in responses {
            if let Ok((mut conn, _)) = listener.accept().await {
                let (headers, body) = read_full_request(&mut conn).await;
                req_index += 1;
                if req_index == 5 {
                    // Complete request
                    let xml = if !body.is_empty() {
                        String::from_utf8_lossy(&body).to_string()
                    } else {
                        String::from_utf8_lossy(&headers).to_string()
                    };
                    let mut guard = captured_clone.lock().expect("lock");
                    *guard = Some(xml);
                }
                let _ = conn.write_all(&resp.into_bytes()).await;
                let _ = conn.flush().await;
                let _ = conn.shutdown().await;
            }
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let store = mock_s3_store(port);
    let mut upload = store.create_multipart_upload("f").await.expect("create");
    // Upload parts out of order: 2, 1, 3
    upload
        .upload_part(2, Bytes::from("b"))
        .await
        .expect("part 2");
    upload
        .upload_part(1, Bytes::from("a"))
        .await
        .expect("part 1");
    upload
        .upload_part(3, Bytes::from("c"))
        .await
        .expect("part 3");
    upload.complete().await.expect("complete");

    let xml_opt = captured_complete_xml.lock().expect("lock");
    let xml = xml_opt.as_deref().expect("complete XML captured");
    // Verify ascending part-number order
    let pos1 = xml
        .find("<PartNumber>1</PartNumber>")
        .expect("part 1 in xml");
    let pos2 = xml
        .find("<PartNumber>2</PartNumber>")
        .expect("part 2 in xml");
    let pos3 = xml
        .find("<PartNumber>3</PartNumber>")
        .expect("part 3 in xml");
    assert!(
        pos1 < pos2 && pos2 < pos3,
        "parts must be in ascending order in CompleteMultipartUpload XML"
    );
}

/// Presigned GET URL contains required X-Amz-* query parameters.
#[tokio::test]
async fn s3_presign_get_url_well_formed() {
    let store = mock_s3_store(0); // port doesn't matter for presigning (offline)
    let url = store
        .presign_get("myobj", std::time::Duration::from_secs(3600))
        .expect("presign_get");
    assert!(
        url.contains("X-Amz-Algorithm"),
        "missing X-Amz-Algorithm: {url}"
    );
    assert!(
        url.contains("X-Amz-Expires"),
        "missing X-Amz-Expires: {url}"
    );
    assert!(
        url.contains("X-Amz-Signature"),
        "missing X-Amz-Signature: {url}"
    );
}

/// Presigned PUT URL contains required X-Amz-* query parameters.
#[tokio::test]
async fn s3_presign_put_url_well_formed() {
    let store = mock_s3_store(0);
    let url = store
        .presign_put(
            "myobj",
            std::time::Duration::from_secs(3600),
            Some("application/octet-stream"),
        )
        .expect("presign_put");
    assert!(
        url.contains("X-Amz-Algorithm"),
        "missing X-Amz-Algorithm: {url}"
    );
    assert!(
        url.contains("X-Amz-Signature"),
        "missing X-Amz-Signature: {url}"
    );
}

/// Server-side copy: the PUT request must include `x-amz-copy-source` header.
#[tokio::test]
async fn s3_copy_request_has_copy_source_header() {
    let capture = Arc::new(Mutex::new(Vec::new()));

    let responses = vec![MockResponse::xml_body(
        200,
        b"<?xml version=\"1.0\"?><CopyObjectResult><LastModified>2026-01-01T00:00:00Z</LastModified><ETag>\"abc\"</ETag></CopyObjectResult>".as_slice(),
    )];
    let port = spawn_mock(responses, capture.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let store = mock_s3_store(port);
    store.copy("src-key", "dst-key").await.expect("copy");

    let reqs = capture.lock().expect("lock");
    let req = reqs.first().expect("one request captured");
    assert!(
        req.headers.contains_key("x-amz-copy-source"),
        "x-amz-copy-source header missing, headers: {:?}",
        req.headers
    );
    let copy_source = req.headers.get("x-amz-copy-source").expect("header");
    assert!(
        copy_source.contains("src-key"),
        "x-amz-copy-source should include src-key: {copy_source}"
    );
}

/// List pagination: mock returns a truncated page, then a final page; all keys collected.
#[tokio::test]
async fn s3_list_pagination_yields_all_keys() {
    let capture = Arc::new(Mutex::new(Vec::new()));

    let page1_xml = br#"<?xml version="1.0"?>
<ListBucketResult>
  <IsTruncated>true</IsTruncated>
  <NextContinuationToken>token-page-2</NextContinuationToken>
  <Contents><Key>key1</Key></Contents>
  <Contents><Key>key2</Key></Contents>
</ListBucketResult>"#;

    let page2_xml = br#"<?xml version="1.0"?>
<ListBucketResult>
  <IsTruncated>false</IsTruncated>
  <Contents><Key>key3</Key></Contents>
  <Contents><Key>key4</Key></Contents>
</ListBucketResult>"#;

    let responses = vec![
        MockResponse::xml_body(200, page1_xml.as_slice()),
        MockResponse::xml_body(200, page2_xml.as_slice()),
    ];
    let port = spawn_mock(responses, capture.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let store = mock_s3_store(port);
    let keys = store.list("").await.expect("list");

    assert_eq!(
        keys,
        vec!["key1", "key2", "key3", "key4"],
        "all pages must be accumulated"
    );
}

/// Retry on 503: mock returns 503 twice then 200; the operation should succeed.
#[tokio::test]
async fn s3_retry_503_then_success() {
    let capture = Arc::new(Mutex::new(Vec::new()));

    let responses = vec![
        MockResponse::error(503), // attempt 1 → retryable
        MockResponse::error(503), // attempt 2 → retryable
        MockResponse::ok_empty(), // attempt 3 → success
    ];
    let port = spawn_mock(responses, capture.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Use a retry config with very short delays for fast tests
    let retry = RetryConfig {
        max_attempts: 3,
        base_delay_ms: 5,
        max_delay_ms: 50,
    };
    let store = mock_s3_store_with_retry(port, retry);
    store
        .put("retrykey", Bytes::from("data"))
        .await
        .expect("put should succeed after retries");

    let reqs = capture.lock().expect("lock");
    assert_eq!(
        reqs.len(),
        3,
        "expected 3 total attempts (2 × 503 + 1 × 200), got {}",
        reqs.len()
    );
}

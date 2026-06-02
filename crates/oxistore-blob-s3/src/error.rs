//! S3 XML error response parser.
//!
//! AWS S3 returns errors as XML bodies of the form:
//!
//! ```xml
//! <Error>
//!   <Code>NoSuchKey</Code>
//!   <Message>The specified key does not exist.</Message>
//!   ...
//! </Error>
//! ```
//!
//! This module parses that structure and converts known error codes to
//! [`BlobError`] variants.

use oxistore_blob::BlobError;
use quick_xml::events::Event;
use quick_xml::Reader;

/// Parsed S3 XML error response.
#[derive(Debug, Default)]
pub struct S3ErrorResponse {
    /// S3 error code, e.g. `"NoSuchKey"`.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

impl S3ErrorResponse {
    /// Parse an S3 XML error body.
    ///
    /// Returns `None` when the body is empty or cannot be parsed as a valid
    /// S3 error response.
    pub fn parse(xml: &[u8]) -> Option<Self> {
        if xml.is_empty() {
            return None;
        }
        let mut reader = Reader::from_reader(xml);
        reader.config_mut().trim_text(true);

        let mut resp = S3ErrorResponse::default();
        let mut current_tag = String::new();
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                }
                Ok(Event::Text(e)) => {
                    let text = e.decode().unwrap_or_default().into_owned();
                    match current_tag.as_str() {
                        "Code" => resp.code = text,
                        "Message" => resp.message = text,
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        if resp.code.is_empty() {
            None
        } else {
            Some(resp)
        }
    }
}

/// Convert an HTTP response (status + body) to a [`BlobError`].
///
/// * HTTP 404 with `NoSuchKey` or `NoSuchBucket` code → `BlobError::NotFound`
/// * Other 4xx/5xx → `BlobError::Other` with the S3 error code and message
pub fn http_error_to_blob_error(status: u16, body: &[u8], key: &str) -> BlobError {
    if let Some(s3_err) = S3ErrorResponse::parse(body) {
        if status == 404 && (s3_err.code == "NoSuchKey" || s3_err.code == "NoSuchBucket") {
            return BlobError::NotFound(key.to_string());
        }
        return BlobError::Other(format!(
            "S3 error {status} {}: {}",
            s3_err.code, s3_err.message
        ));
    }
    // No parseable XML — use raw status
    if status == 404 {
        BlobError::NotFound(key.to_string())
    } else {
        BlobError::Other(format!(
            "S3 HTTP {status} for key {key}: {}",
            String::from_utf8_lossy(body)
        ))
    }
}

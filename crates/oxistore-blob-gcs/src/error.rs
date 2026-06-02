//! Error types for the GCS BlobStore adapter.

use oxistore_blob::BlobError;

/// Errors produced by GCS operations before conversion to [`BlobError`].
#[derive(Debug)]
pub enum GcsError {
    /// HTTP-level error (non-success status or transport failure).
    Http(String),
    /// Authentication / JWT construction failure.
    Auth(String),
    /// JSON serialisation or deserialisation failure.
    Json(serde_json::Error),
    /// The requested object does not exist (GCS 404).
    NotFound,
    /// Any other unexpected failure.
    Other(String),
}

impl std::fmt::Display for GcsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GcsError::Http(msg) => write!(f, "gcs http error: {msg}"),
            GcsError::Auth(msg) => write!(f, "gcs auth error: {msg}"),
            GcsError::Json(e) => write!(f, "gcs json error: {e}"),
            GcsError::NotFound => write!(f, "gcs object not found"),
            GcsError::Other(msg) => write!(f, "gcs error: {msg}"),
        }
    }
}

impl std::error::Error for GcsError {}

impl From<GcsError> for BlobError {
    fn from(e: GcsError) -> Self {
        match e {
            GcsError::NotFound => BlobError::NotFound("gcs: object not found".to_string()),
            GcsError::Auth(msg) => BlobError::Other(format!("gcs auth: {msg}")),
            GcsError::Http(msg) => BlobError::Other(format!("gcs http: {msg}")),
            GcsError::Json(e) => BlobError::Other(format!("gcs json: {e}")),
            GcsError::Other(msg) => BlobError::Other(msg),
        }
    }
}

impl From<serde_json::Error> for GcsError {
    fn from(e: serde_json::Error) -> Self {
        GcsError::Json(e)
    }
}

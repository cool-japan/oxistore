//! Azure-specific error types and mapping to [`BlobError`].

use oxistore_blob::BlobError;

/// Errors produced by the Azure Blob Storage backend.
#[derive(Debug)]
pub enum AzureError {
    /// Authentication or credential error (e.g. invalid account key).
    Auth(String),
    /// An HTTP-level error (status code, network, etc.).
    Http(String),
    /// Unexpected or unparseable server response.
    Response(String),
    /// XML parsing error (list blobs, etc.).
    Xml(String),
    /// The requested blob was not found (HTTP 404).
    NotFound(String),
}

impl std::fmt::Display for AzureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AzureError::Auth(msg) => write!(f, "Azure auth error: {msg}"),
            AzureError::Http(msg) => write!(f, "Azure HTTP error: {msg}"),
            AzureError::Response(msg) => write!(f, "Azure response error: {msg}"),
            AzureError::Xml(msg) => write!(f, "Azure XML error: {msg}"),
            AzureError::NotFound(key) => write!(f, "Azure blob not found: {key}"),
        }
    }
}

impl std::error::Error for AzureError {}

impl From<AzureError> for BlobError {
    fn from(e: AzureError) -> Self {
        match e {
            AzureError::NotFound(key) => BlobError::NotFound(key),
            AzureError::Auth(msg) => BlobError::Other(format!("azure auth: {msg}")),
            AzureError::Http(msg) => BlobError::Other(format!("azure http: {msg}")),
            AzureError::Response(msg) => BlobError::Other(format!("azure response: {msg}")),
            AzureError::Xml(msg) => BlobError::Other(format!("azure xml: {msg}")),
        }
    }
}

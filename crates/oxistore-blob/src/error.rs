/// Errors returned by any [`crate::BlobStore`] implementation.
#[non_exhaustive]
#[derive(Debug)]
pub enum BlobError {
    /// The requested key was not found in the store.
    NotFound(String),
    /// The key already exists (for conditional put operations).
    AlreadyExists(String),
    /// An I/O error occurred at the file-system level.
    Io(std::io::Error),
    /// A checksum verification failed.
    ChecksumMismatch(String),
    /// Any other store-specific error.
    Other(String),
    /// The operation would exceed the store's configured storage quota.
    QuotaExceeded {
        /// The maximum allowed bytes.
        limit_bytes: u64,
        /// The bytes required by the operation that triggered the error.
        needed_bytes: u64,
    },
    /// A multipart upload operation failed (e.g., state error, missing ETag).
    MultipartError(String),
    /// All retry attempts were exhausted without a successful response.
    RetryExhausted {
        /// Total number of attempts made.
        attempts: u32,
        /// Human-readable description of the last error.
        last_error: String,
    },
}

impl std::fmt::Display for BlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlobError::NotFound(key) => write!(f, "key not found: {key}"),
            BlobError::AlreadyExists(key) => write!(f, "key already exists: {key}"),
            BlobError::Io(e) => write!(f, "I/O error: {e}"),
            BlobError::ChecksumMismatch(msg) => write!(f, "checksum mismatch: {msg}"),
            BlobError::Other(msg) => write!(f, "{msg}"),
            BlobError::QuotaExceeded {
                limit_bytes,
                needed_bytes,
            } => write!(
                f,
                "storage quota exceeded: limit {limit_bytes} bytes, needed {needed_bytes} bytes"
            ),
            BlobError::MultipartError(msg) => write!(f, "multipart error: {msg}"),
            BlobError::RetryExhausted {
                attempts,
                last_error,
            } => {
                write!(f, "retry exhausted after {attempts} attempts: {last_error}")
            }
        }
    }
}

impl std::error::Error for BlobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BlobError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for BlobError {
    fn from(e: std::io::Error) -> Self {
        BlobError::Io(e)
    }
}

impl From<BlobError> for oxistore_core::StoreError {
    /// Convert a [`BlobError`] into a [`oxistore_core::StoreError`].
    ///
    /// This allows blob operations to propagate cleanly through functions
    /// that return `StoreError`.
    fn from(e: BlobError) -> Self {
        match e {
            BlobError::NotFound(k) => {
                oxistore_core::StoreError::Other(format!("blob not found: {k}"))
            }
            BlobError::AlreadyExists(_k) => oxistore_core::StoreError::AlreadyExists,
            BlobError::Io(io_err) => oxistore_core::StoreError::Io(std::sync::Arc::new(io_err)),
            BlobError::ChecksumMismatch(msg) => {
                oxistore_core::StoreError::Corruption(format!("checksum mismatch: {msg}"))
            }
            BlobError::QuotaExceeded { .. } => oxistore_core::StoreError::CapacityExceeded,
            other => oxistore_core::StoreError::Other(other.to_string()),
        }
    }
}

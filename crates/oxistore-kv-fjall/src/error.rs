/// Errors specific to the fjall backend.
///
/// These are wrapped into [`oxistore_core::StoreError`] at the trait boundary;
/// this type is exposed for callers that open a [`crate::FjallStore`] directly.
#[derive(Debug)]
pub enum FjallStoreError {
    /// The database or keyspace could not be opened.
    Open(String),
    /// A read error occurred.
    Read(String),
    /// A write error occurred.
    Write(String),
    /// An error occurred while persisting the journal.
    Persist(String),
}

impl std::fmt::Display for FjallStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FjallStoreError::Open(s) => write!(f, "fjall open error: {s}"),
            FjallStoreError::Read(s) => write!(f, "fjall read error: {s}"),
            FjallStoreError::Write(s) => write!(f, "fjall write error: {s}"),
            FjallStoreError::Persist(s) => write!(f, "fjall persist error: {s}"),
        }
    }
}

impl std::error::Error for FjallStoreError {}

impl From<FjallStoreError> for oxistore_core::StoreError {
    fn from(e: FjallStoreError) -> Self {
        oxistore_core::StoreError::Other(e.to_string())
    }
}

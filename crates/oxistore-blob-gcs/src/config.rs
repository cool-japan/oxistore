//! Configuration types for the GCS BlobStore adapter.

use crate::error::GcsError;
use std::path::Path;
use std::time::Duration;

/// Google service account credentials loaded from a JSON key file.
///
/// The PEM private key must be in PKCS#8 format (`-----BEGIN PRIVATE KEY-----`).
#[derive(Clone)]
pub struct GcsServiceAccount {
    /// The service account email address (`client_email` field in the JSON key).
    pub client_email: String,
    /// The RSA private key in PEM format (newlines as `\n`, not `\\n`).
    pub private_key_pem: String,
    /// Optional GCP project ID.
    pub project_id: Option<String>,
    /// The OAuth2 token endpoint URI.
    ///
    /// Default: `"https://oauth2.googleapis.com/token"`.
    pub token_uri: String,
}

impl std::fmt::Debug for GcsServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcsServiceAccount")
            .field("client_email", &self.client_email)
            .field("private_key_pem", &"<redacted>")
            .field("project_id", &self.project_id)
            .field("token_uri", &self.token_uri)
            .finish()
    }
}

impl GcsServiceAccount {
    /// Load credentials from a Google service account JSON key file.
    ///
    /// Accepts the standard JSON format produced by `gcloud iam service-accounts
    /// keys create`.  The `private_key` field may use literal `\\n` sequences
    /// (as produced by the Google Console JSON download) or real newlines.
    pub fn from_json_file(path: impl AsRef<Path>) -> Result<Self, GcsError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| GcsError::Auth(format!("cannot read service account file: {e}")))?;
        Self::from_json_str(&content)
    }

    /// Parse credentials from a JSON string.
    pub fn from_json_str(json: &str) -> Result<Self, GcsError> {
        let v: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| GcsError::Auth(format!("invalid service account JSON: {e}")))?;

        let client_email = v["client_email"]
            .as_str()
            .ok_or_else(|| GcsError::Auth("missing client_email field".to_string()))?
            .to_string();

        let raw_key = v["private_key"]
            .as_str()
            .ok_or_else(|| GcsError::Auth("missing private_key field".to_string()))?;

        // Normalise: replace literal `\n` (two characters) with actual newlines
        let private_key_pem = raw_key.replace("\\n", "\n");

        let project_id = v["project_id"].as_str().map(str::to_string);

        let token_uri = v["token_uri"]
            .as_str()
            .unwrap_or("https://oauth2.googleapis.com/token")
            .to_string();

        Ok(Self {
            client_email,
            private_key_pem,
            project_id,
            token_uri,
        })
    }

    /// Load credentials from the path pointed to by `GOOGLE_APPLICATION_CREDENTIALS`.
    pub fn from_env() -> Result<Self, GcsError> {
        let path = std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
            .map_err(|_| GcsError::Auth("GOOGLE_APPLICATION_CREDENTIALS not set".to_string()))?;
        Self::from_json_file(path)
    }
}

/// Configuration for [`crate::GcsBlobStore`].
#[derive(Clone, Debug)]
pub struct GcsConfig {
    /// The GCS bucket name.
    pub bucket: String,
    /// Service account credentials.
    pub credentials: GcsServiceAccount,
    /// Per-request HTTP timeout.
    pub timeout: Duration,
    /// Optional endpoint override (e.g. for testing with a local mock server).
    ///
    /// Defaults to `"https://storage.googleapis.com"`.
    pub endpoint: Option<String>,
    /// Optional OAuth2 token endpoint override (for testing).
    ///
    /// Defaults to the `token_uri` in the service account JSON (typically
    /// `"https://oauth2.googleapis.com/token"`).
    pub oauth_endpoint: Option<String>,
}

impl GcsConfig {
    /// Return the effective storage endpoint (without trailing slash).
    pub fn storage_endpoint(&self) -> &str {
        self.endpoint
            .as_deref()
            .unwrap_or("https://storage.googleapis.com")
    }

    /// Return the effective OAuth2 token endpoint.
    pub fn token_endpoint(&self) -> &str {
        self.oauth_endpoint
            .as_deref()
            .unwrap_or(self.credentials.token_uri.as_str())
    }
}

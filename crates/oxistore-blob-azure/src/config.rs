//! Azure credentials and configuration.

use crate::error::AzureError;

/// Azure Storage account credentials (Shared Key authentication).
#[derive(Debug, Clone)]
pub struct AzureCredentials {
    /// Azure Storage account name.
    pub account_name: String,
    /// Base64-encoded account key (from the Azure portal).
    pub account_key_b64: String,
}

impl AzureCredentials {
    /// Parse credentials from an Azure connection string.
    ///
    /// Expected format:
    /// `DefaultEndpointsProtocol=https;AccountName=...;AccountKey=...;EndpointSuffix=...`
    pub fn from_connection_string(s: &str) -> Result<Self, AzureError> {
        let mut account_name = None::<String>;
        let mut account_key = None::<String>;

        for part in s.split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            // Split only on the first '=' so that base64 values (which may contain '=') are intact.
            if let Some(eq_pos) = part.find('=') {
                let key = &part[..eq_pos];
                let val = &part[eq_pos + 1..];
                match key {
                    "AccountName" => account_name = Some(val.to_string()),
                    "AccountKey" => account_key = Some(val.to_string()),
                    _ => {}
                }
            }
        }

        let account_name = account_name
            .ok_or_else(|| AzureError::Auth("connection string missing AccountName".to_string()))?;
        let account_key_b64 = account_key
            .ok_or_else(|| AzureError::Auth("connection string missing AccountKey".to_string()))?;

        Ok(Self {
            account_name,
            account_key_b64,
        })
    }

    /// Read credentials from environment variables.
    ///
    /// Reads `AZURE_STORAGE_ACCOUNT` and `AZURE_STORAGE_KEY`.
    pub fn from_env() -> Result<Self, AzureError> {
        let account_name = std::env::var("AZURE_STORAGE_ACCOUNT").map_err(|_| {
            AzureError::Auth("environment variable AZURE_STORAGE_ACCOUNT not set".to_string())
        })?;
        let account_key_b64 = std::env::var("AZURE_STORAGE_KEY").map_err(|_| {
            AzureError::Auth("environment variable AZURE_STORAGE_KEY not set".to_string())
        })?;
        Ok(Self {
            account_name,
            account_key_b64,
        })
    }

    /// Decode the account key bytes for HMAC signing.
    pub(crate) fn key_bytes(&self) -> Result<Vec<u8>, AzureError> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(&self.account_key_b64)
            .map_err(|e| AzureError::Auth(format!("invalid account key base64: {e}")))
    }
}

/// Full configuration for an [`AzureBlobStore`](crate::AzureBlobStore).
#[derive(Debug, Clone)]
pub struct AzureConfig {
    /// Credentials (account name + key).
    pub credentials: AzureCredentials,
    /// Target container name.
    pub container: String,
    /// Request timeout.
    pub timeout: std::time::Duration,
    /// Optional endpoint override. Defaults to
    /// `https://<account>.blob.core.windows.net`.
    ///
    /// Set to `http://127.0.0.1:<port>` in tests.
    pub endpoint: Option<String>,
}

impl AzureConfig {
    /// Build a config with sensible defaults (30-second timeout).
    pub fn new(credentials: AzureCredentials, container: impl Into<String>) -> Self {
        Self {
            credentials,
            container: container.into(),
            timeout: std::time::Duration::from_secs(30),
            endpoint: None,
        }
    }
}

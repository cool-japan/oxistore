//! S3 configuration: endpoint, region, bucket, credentials, retry, and builder.

use oxistore_blob::BlobError;

use crate::retry::RetryConfig;

/// AWS credentials for signing S3 requests.
#[derive(Clone)]
pub struct S3Credentials {
    /// AWS Access Key ID
    pub access_key_id: String,
    /// AWS Secret Access Key
    pub secret_access_key: String,
    /// Optional session token (for STS-issued credentials)
    pub session_token: Option<String>,
}

impl S3Credentials {
    /// Create credentials from explicit values.
    pub fn new(
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
        session_token: Option<String>,
    ) -> Self {
        Self {
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
            session_token,
        }
    }

    /// Load credentials from the standard AWS environment variables:
    /// - `AWS_ACCESS_KEY_ID`
    /// - `AWS_SECRET_ACCESS_KEY`
    /// - `AWS_SESSION_TOKEN` (optional)
    pub fn from_env() -> Result<Self, BlobError> {
        Self::from_env_with(|k| std::env::var(k).ok())
    }

    /// Load credentials using a custom environment variable getter.
    ///
    /// Useful for testing without mutating the real process environment.
    pub fn from_env_with<F>(getter: F) -> Result<Self, BlobError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let access_key_id = getter("AWS_ACCESS_KEY_ID").ok_or_else(|| {
            BlobError::Other("AWS_ACCESS_KEY_ID environment variable is not set".to_string())
        })?;
        let secret_access_key = getter("AWS_SECRET_ACCESS_KEY").ok_or_else(|| {
            BlobError::Other("AWS_SECRET_ACCESS_KEY environment variable is not set".to_string())
        })?;
        let session_token = getter("AWS_SESSION_TOKEN");
        Ok(Self {
            access_key_id,
            secret_access_key,
            session_token,
        })
    }
}

impl std::fmt::Debug for S3Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"**REDACTED**")
            .field(
                "session_token",
                &self.session_token.as_deref().map(|_| "**REDACTED**"),
            )
            .finish()
    }
}

/// Full configuration for an S3-compatible blob store.
#[derive(Debug, Clone)]
pub struct S3Config {
    /// Endpoint URL, e.g. `"https://s3.us-east-1.amazonaws.com"` or
    /// `"http://localhost:9000"` for MinIO.
    pub endpoint: String,
    /// AWS region, e.g. `"us-east-1"`.
    pub region: String,
    /// Bucket name.
    pub bucket: String,
    /// Signing credentials.
    pub credentials: S3Credentials,
    /// If `true`, use path-style URLs (`endpoint/bucket/key`).
    /// If `false`, use virtual-host style (`bucket.host/key`).
    pub path_style: bool,
    /// Per-operation timeout in seconds.
    pub timeout_secs: u64,
    /// Retry / backoff configuration.
    pub retry_config: RetryConfig,
}

/// Builder for [`S3Config`] and the resulting [`crate::S3BlobStore`].
#[derive(Debug, Default)]
pub struct S3BlobStoreBuilder {
    endpoint: Option<String>,
    region: Option<String>,
    bucket: Option<String>,
    credentials: Option<S3Credentials>,
    path_style: bool,
    timeout_secs: u64,
    retry_config: RetryConfig,
}

impl S3BlobStoreBuilder {
    /// Create a new builder with defaults (path-style, 30 s timeout).
    pub fn new() -> Self {
        Self {
            path_style: true,
            timeout_secs: 30,
            retry_config: RetryConfig::default(),
            ..Self::default()
        }
    }

    /// Set the S3-compatible endpoint URL.
    pub fn endpoint(mut self, e: impl Into<String>) -> Self {
        self.endpoint = Some(e.into());
        self
    }

    /// Set the AWS region.
    pub fn region(mut self, r: impl Into<String>) -> Self {
        self.region = Some(r.into());
        self
    }

    /// Set the bucket name.
    pub fn bucket(mut self, b: impl Into<String>) -> Self {
        self.bucket = Some(b.into());
        self
    }

    /// Set the credentials.
    pub fn credentials(mut self, c: S3Credentials) -> Self {
        self.credentials = Some(c);
        self
    }

    /// Enable or disable path-style URLs (default: `true`).
    pub fn path_style(mut self, p: bool) -> Self {
        self.path_style = p;
        self
    }

    /// Set the per-operation timeout in seconds (default: 30).
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Override the retry / backoff configuration.
    pub fn retry_config(mut self, cfg: RetryConfig) -> Self {
        self.retry_config = cfg;
        self
    }

    /// Build the [`crate::S3BlobStore`].
    pub fn build(self) -> Result<crate::S3BlobStore, BlobError> {
        let endpoint = self.endpoint.ok_or_else(|| {
            BlobError::Other("S3BlobStoreBuilder: endpoint is required".to_string())
        })?;
        let region = self.region.ok_or_else(|| {
            BlobError::Other("S3BlobStoreBuilder: region is required".to_string())
        })?;
        let bucket = self.bucket.ok_or_else(|| {
            BlobError::Other("S3BlobStoreBuilder: bucket is required".to_string())
        })?;
        let credentials = self.credentials.ok_or_else(|| {
            BlobError::Other("S3BlobStoreBuilder: credentials are required".to_string())
        })?;

        let config = S3Config {
            endpoint,
            region,
            bucket,
            credentials,
            path_style: self.path_style,
            timeout_secs: self.timeout_secs,
            retry_config: self.retry_config,
        };

        crate::S3BlobStore::new(config)
    }
}

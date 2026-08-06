//! Presigned URL generation for S3 objects.
//!
//! Presigned URLs are generated offline (no HTTP round-trip).  They embed
//! SigV4 signing parameters as query parameters, allowing any HTTP client
//! to perform the operation without AWS credentials.
//!
//! # Implementation note
//!
//! We use `aws_sigv4::http_request::sign` with `SigningSettings { expires_in,
//! signature_location: SignatureLocation::QueryParams, .. }` to produce
//! query-string signed URLs.
//!
//! The returned URL contains:
//! - `X-Amz-Algorithm`
//! - `X-Amz-Credential`
//! - `X-Amz-Date`
//! - `X-Amz-Expires`
//! - `X-Amz-SignedHeaders`
//! - `X-Amz-Signature`

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    sign, PercentEncodingMode, SignableBody, SignableRequest, SignatureLocation, SigningSettings,
    UriPathNormalizationMode,
};
use aws_sigv4::sign::v4;
use oxistore_blob::BlobError;
use std::time::{Duration, SystemTime};

use crate::S3BlobStore;

impl S3BlobStore {
    /// Generate a presigned GET URL for the given key.
    ///
    /// The URL is valid for `ttl` from now.  Anyone with the URL can GET the
    /// object without AWS credentials.
    pub fn presign_get(&self, key: &str, ttl: Duration) -> Result<String, BlobError> {
        self.presign_impl("GET", key, ttl, None)
    }

    /// Generate a presigned PUT URL for the given key.
    ///
    /// The URL is valid for `ttl` from now.  If `content_type` is provided it
    /// is included in the signed headers (the caller must send the same
    /// `Content-Type` when using the URL).
    pub fn presign_put(
        &self,
        key: &str,
        ttl: Duration,
        content_type: Option<&str>,
    ) -> Result<String, BlobError> {
        self.presign_impl("PUT", key, ttl, content_type)
    }

    /// Internal presign implementation.
    fn presign_impl(
        &self,
        method: &str,
        key: &str,
        ttl: Duration,
        content_type: Option<&str>,
    ) -> Result<String, BlobError> {
        let uri = self.object_url(key)?;
        let host_header = Self::url_host_header(&uri)?;

        let mut headers: Vec<(&'static str, String)> = vec![("host", host_header.clone())];

        if let Some(ct) = content_type {
            headers.push(("content-type", ct.to_string()));
        }

        let header_refs: Vec<(&str, &str)> =
            headers.iter().map(|(k, v)| (*k, v.as_str())).collect();

        let creds = Credentials::new(
            &self.config.credentials.access_key_id,
            &self.config.credentials.secret_access_key,
            self.config.credentials.session_token.clone(),
            None,
            "oxistore-blob-s3",
        );
        let identity = creds.into();

        let mut settings = SigningSettings::default();
        settings.signature_location = SignatureLocation::QueryParams;
        settings.expires_in = Some(ttl);
        // Same `Single`/`Disabled` correction as header signing (see
        // sigv4::sign_request) so the canonical path used for the signature
        // matches the literal, single-encoded wire path from `object_url()`.
        // `payload_checksum_kind` is intentionally left at its `NoHeader`
        // default: presigned requests sign `SignableBody::UnsignedPayload`
        // ("UNSIGNED-PAYLOAD"), and `x-amz-content-sha256` is only emitted as
        // a *header* for `SignatureLocation::Headers` requests, so it has no
        // effect here.
        settings.percent_encoding_mode = PercentEncodingMode::Single;
        settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;

        let signing_params: aws_sigv4::http_request::SigningParams = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.config.region)
            .name("s3")
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|e| BlobError::Other(format!("presign: sigv4 params build: {e}")))?
            .into();

        let signable = SignableRequest::new(
            method,
            &uri,
            header_refs.iter().copied(),
            SignableBody::UnsignedPayload,
        )
        .map_err(|e| BlobError::Other(format!("presign: signable request: {e}")))?;

        let (signing_instructions, _signature) = sign(signable, &signing_params)
            .map_err(|e| BlobError::Other(format!("presign: signing failed: {e}")))?
            .into_parts();

        // Build the presigned URL by appending query parameters
        let params: Vec<(String, String)> = signing_instructions
            .params()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        let mut presigned_url = uri;
        if !params.is_empty() {
            presigned_url.push('?');
            let qs: String = params
                .iter()
                .map(|(k, v)| format!("{}={}", k, crate::percent_encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            presigned_url.push_str(&qs);
        }

        Ok(presigned_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{S3BlobStoreBuilder, S3Credentials};

    fn test_store() -> S3BlobStore {
        S3BlobStoreBuilder::new()
            .endpoint("http://localhost:9000")
            .region("us-east-1")
            .bucket("testbucket")
            .credentials(S3Credentials::new("AKIATEST", "SECRETTEST", None))
            .path_style(true)
            .build()
            .expect("build store")
    }

    #[test]
    fn presign_get_contains_amz_params() {
        let store = test_store();
        let url = store
            .presign_get("mykey", Duration::from_secs(3600))
            .expect("presign get");
        assert!(
            url.contains("X-Amz-Algorithm"),
            "should contain X-Amz-Algorithm: {url}"
        );
        assert!(
            url.contains("X-Amz-Signature"),
            "should contain X-Amz-Signature: {url}"
        );
        assert!(
            url.contains("X-Amz-Expires"),
            "should contain X-Amz-Expires: {url}"
        );
    }

    /// Regression test for the presign half of the `SigningSettings::default()`
    /// fix: the canonical path signed for a presigned URL must be
    /// *single*-encoded, matching the literal, single-encoded wire path
    /// produced by `object_url()` — the `Double` default would sign
    /// `/testbucket/my%2520file.txt` while the actual URL stays
    /// `/testbucket/my%20file.txt`, and real S3 would reject the resulting
    /// presigned URL with `SignatureDoesNotMatch`. Presigning is entirely
    /// offline (no HTTP round-trip), so this needs no mock server.
    #[test]
    fn presign_get_path_is_single_encoded() {
        let store = test_store();
        let url = store
            .presign_get("my file.txt", Duration::from_secs(3600))
            .expect("presign get");

        // Assert on the path portion (before the `?`) only, so this doesn't
        // couple to how query parameters happen to be encoded.
        let path = url.split('?').next().expect("url has a path portion");
        assert!(
            path.contains("/testbucket/my%20file.txt"),
            "expected single-encoded space in presigned path, got: {path}"
        );
        assert!(
            !path.contains("%2520"),
            "presigned path must not be double-encoded: {path}"
        );
    }

    #[test]
    fn presign_put_contains_amz_params() {
        let store = test_store();
        let url = store
            .presign_put(
                "mykey",
                Duration::from_secs(3600),
                Some("application/octet-stream"),
            )
            .expect("presign put");
        assert!(
            url.contains("X-Amz-Algorithm"),
            "should contain X-Amz-Algorithm: {url}"
        );
        assert!(
            url.contains("X-Amz-Signature"),
            "should contain X-Amz-Signature: {url}"
        );
    }
}

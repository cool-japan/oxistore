//! SigV4 signing helpers that wrap `aws_sigv4::http_request`.
//!
//! `sign_request` mutates the caller-supplied header list in-place, appending
//! the `x-amz-date`, `authorization`, and (when present) `x-amz-security-token`
//! headers produced by the aws-sigv4 crate.
//!
//! The `aws_sigv4` crate handles all canonical request + HMAC-SHA256 steps
//! internally.  This module is a thin adapter that maps our domain types to
//! the aws-sigv4 API surface.

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4;
use oxistore_blob::BlobError;
use std::time::SystemTime;

use crate::config::S3Credentials;

/// Sign the HTTP request described by `method`, `uri`, and existing `headers`.
///
/// Returns a list of additional headers that must be injected into the
/// outgoing request.  The list always contains at least `x-amz-date` and
/// `authorization`; it may also contain `x-amz-content-sha256` and
/// `x-amz-security-token`.
///
/// # Arguments
///
/// * `method` — HTTP method in upper-case, e.g. `"GET"`.
/// * `uri`    — Full URI with scheme and authority.
/// * `headers` — Existing request headers as `(name, value)` pairs.
///   These are included in the signed-headers list, so `host` must be present.
/// * `body`   — Raw request body (may be empty).
/// * `credentials` — AWS signing credentials.
/// * `region` — AWS region string.
pub fn sign_request(
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    credentials: &S3Credentials,
    region: &str,
) -> Result<Vec<(String, String)>, BlobError> {
    let creds = Credentials::new(
        &credentials.access_key_id,
        &credentials.secret_access_key,
        credentials.session_token.clone(),
        None,
        "oxistore-blob-s3",
    );

    let identity = creds.into();

    let signing_settings = SigningSettings::default();

    let signing_params: aws_sigv4::http_request::SigningParams = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name("s3")
        .time(SystemTime::now())
        .settings(signing_settings)
        .build()
        .map_err(|e| BlobError::Other(format!("sigv4 params build: {e}")))?
        .into();

    let signable = SignableRequest::new(
        method,
        uri,
        headers.iter().copied(),
        SignableBody::Bytes(body),
    )
    .map_err(|e| BlobError::Other(format!("sigv4 signable request: {e}")))?;

    let (signing_instructions, _signature) = sign(signable, &signing_params)
        .map_err(|e| BlobError::Other(format!("sigv4 signing failed: {e}")))?
        .into_parts();

    let extra_headers: Vec<(String, String)> = signing_instructions
        .headers()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    Ok(extra_headers)
}

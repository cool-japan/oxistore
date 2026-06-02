//! SigV4 reference unit tests.
//!
//! These tests verify that `sign_request` produces Authorization headers
//! with the correct algorithm prefix and structure without requiring a live
//! S3 endpoint.

use oxistore_blob_s3::config::S3Credentials;
use oxistore_blob_s3::sigv4::sign_request;

/// Signing a GET request must produce an Authorization header starting with
/// `"AWS4-HMAC-SHA256 "` and containing the expected fields.
#[test]
fn sigv4_authorization_header_well_formed() {
    let creds = S3Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        None,
    );

    let method = "GET";
    let uri = "http://examplebucket.s3.amazonaws.com/test.txt";
    let headers = [("host", "examplebucket.s3.amazonaws.com:80")];

    let signed = sign_request(method, uri, &headers, &[], &creds, "us-east-1")
        .expect("signing must not fail");

    // Find the authorization header
    let auth = signed
        .iter()
        .find(|(k, _)| k.to_lowercase() == "authorization")
        .map(|(_, v)| v.as_str())
        .expect("authorization header must be present");

    assert!(
        auth.starts_with("AWS4-HMAC-SHA256 "),
        "Authorization must start with 'AWS4-HMAC-SHA256 ', got: {auth}"
    );
    assert!(
        auth.contains("Credential="),
        "Authorization must contain 'Credential=', got: {auth}"
    );
    assert!(
        auth.contains("SignedHeaders="),
        "Authorization must contain 'SignedHeaders=', got: {auth}"
    );
    assert!(
        auth.contains("Signature="),
        "Authorization must contain 'Signature=', got: {auth}"
    );
}

/// Signing with a session token must include `x-amz-security-token` in the
/// header output.
#[test]
fn sigv4_with_session_token_includes_security_token_header() {
    let creds = S3Credentials::new(
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        Some("sessiontoken123".to_string()),
    );

    let signed = sign_request(
        "PUT",
        "http://localhost:9000/bucket/file.txt",
        &[("host", "localhost:9000")],
        b"payload",
        &creds,
        "us-east-1",
    )
    .expect("signing must not fail");

    let has_sts = signed
        .iter()
        .any(|(k, _)| k.to_lowercase() == "x-amz-security-token");

    assert!(
        has_sts,
        "x-amz-security-token header must be present when session_token is set"
    );
}

/// Signing must include an `x-amz-date` header.
#[test]
fn sigv4_includes_amz_date_header() {
    let creds = S3Credentials::new("KEY", "SECRET", None);

    let signed = sign_request(
        "HEAD",
        "http://localhost:9000/bucket/key",
        &[("host", "localhost:9000")],
        &[],
        &creds,
        "eu-west-1",
    )
    .expect("signing must not fail");

    let has_date = signed.iter().any(|(k, _)| k.to_lowercase() == "x-amz-date");

    assert!(
        has_date,
        "x-amz-date header must be present in signed output"
    );
}

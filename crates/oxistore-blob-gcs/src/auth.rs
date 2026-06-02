//! JWT construction, RS256 signing, and OAuth2 token exchange with caching.
//!
//! Implements the Google OAuth2 Service Account flow:
//! 1. Build a JWT with RS256 (RFC 7519 / RFC 7515).
//! 2. POST the JWT to the token endpoint as a Bearer-assertion grant.
//! 3. Cache the returned Bearer token until 60 s before expiry.

use crate::config::GcsServiceAccount;
use crate::error::GcsError;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use oxicrypto_sig::RsaPkcs1v15Sha256Signer;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

// ── PEM stripping ─────────────────────────────────────────────────────────────

/// Decode a PKCS#8 PEM private key to raw DER bytes.
///
/// Accepts `-----BEGIN PRIVATE KEY-----` (PKCS#8) and
/// `-----BEGIN RSA PRIVATE KEY-----` (PKCS#1 / legacy — not supported by our
/// signing stack, so we reject it with an explanatory error).
pub fn pem_to_der(pem: &str) -> Result<Vec<u8>, GcsError> {
    // Strip header/footer lines and any whitespace, then base64-decode.
    let mut b64 = String::new();
    let mut in_body = false;
    for line in pem.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("-----BEGIN") {
            if trimmed.contains("RSA PRIVATE KEY") {
                return Err(GcsError::Auth(
                    "PKCS#1 RSA private keys are not supported; \
                     convert with: openssl pkcs8 -topk8 -nocrypt -in key.pem -out key_pkcs8.pem"
                        .to_string(),
                ));
            }
            in_body = true;
            continue;
        }
        if trimmed.starts_with("-----END") {
            break;
        }
        if in_body {
            b64.push_str(trimmed);
        }
    }
    if !in_body {
        return Err(GcsError::Auth("PEM key block not found".to_string()));
    }
    // Use standard base64 (PEM uses standard alphabet with padding)
    base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .map_err(|e| GcsError::Auth(format!("PEM base64 decode failed: {e}")))
}

// ── JWT construction ──────────────────────────────────────────────────────────

/// Build an RS256-signed JWT for the Google OAuth2 service account flow.
///
/// Returns the compact serialisation `header.claims.signature`.
pub fn build_jwt(sa: &GcsServiceAccount, audience: &str) -> Result<String, GcsError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| GcsError::Auth(format!("system clock error: {e}")))?
        .as_secs();

    // JWT header (RFC 7515 §4.1)
    let header = r#"{"alg":"RS256","typ":"JWT"}"#;
    let header_b64 = URL_SAFE_NO_PAD.encode(header.as_bytes());

    // JWT claims (RFC 7519 §4)
    let claims = format!(
        r#"{{"iss":"{iss}","scope":"https://www.googleapis.com/auth/devstorage.read_write","aud":"{aud}","exp":{exp},"iat":{iat}}}"#,
        iss = sa.client_email,
        aud = audience,
        exp = now + 3600,
        iat = now,
    );
    let claims_b64 = URL_SAFE_NO_PAD.encode(claims.as_bytes());

    // Signing input: ASCII(header_b64) + "." + ASCII(claims_b64)
    let signing_input = format!("{header_b64}.{claims_b64}");

    // Parse PKCS#8 DER and sign
    let der = pem_to_der(&sa.private_key_pem)?;
    let signer = RsaPkcs1v15Sha256Signer::from_pkcs8_der(&der)
        .map_err(|e| GcsError::Auth(format!("RSA key parse failed: {e:?}")))?;
    let sig_bytes = signer
        .sign(signing_input.as_bytes())
        .map_err(|e| GcsError::Auth(format!("RSA sign failed: {e:?}")))?;

    let sig_b64 = URL_SAFE_NO_PAD.encode(&sig_bytes);
    Ok(format!("{signing_input}.{sig_b64}"))
}

// ── Token exchange ────────────────────────────────────────────────────────────

#[derive(serde::Deserialize, Debug)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
}

/// Cached OAuth2 Bearer token with expiry tracking.
#[derive(Default)]
pub struct TokenCache {
    inner: Arc<Mutex<Option<(String, Instant)>>>,
}

impl TokenCache {
    /// Create an empty token cache.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Return a valid Bearer token, refreshing if needed.
    ///
    /// The token is considered stale when fewer than 60 seconds remain until
    /// expiry.
    pub async fn get_or_refresh(
        &self,
        sa: &GcsServiceAccount,
        http_client: &oxihttp_client::HttpsClient,
        token_uri: &str,
    ) -> Result<String, GcsError> {
        let mut guard = self.inner.lock().await;

        // Check cached token
        if let Some((ref token, expiry)) = *guard {
            if Instant::now() + Duration::from_secs(60) < expiry {
                return Ok(token.clone());
            }
        }

        // Build JWT and exchange for Bearer token
        let jwt = build_jwt(sa, token_uri)?;

        let form_body = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={jwt}"
        );

        let resp = http_client
            .post(token_uri)
            .map_err(|e| GcsError::Http(format!("build POST request: {e}")))?
            .header("Content-Type", "application/x-www-form-urlencoded")
            .map_err(|e| GcsError::Http(format!("set content-type: {e}")))?
            .body(form_body.into_bytes())
            .send()
            .await
            .map_err(|e| GcsError::Http(format!("token exchange POST: {e}")))?;

        let status = resp.status().as_u16();
        let body = resp
            .body_bytes()
            .await
            .map_err(|e| GcsError::Http(format!("read token response body: {e}")))?;

        if status != 200 {
            return Err(GcsError::Auth(format!(
                "token endpoint returned {status}: {}",
                String::from_utf8_lossy(&body)
            )));
        }

        let token_resp: TokenResponse = serde_json::from_slice(&body).map_err(GcsError::Json)?;

        let ttl = token_resp.expires_in.unwrap_or(3600);
        let expiry = Instant::now() + Duration::from_secs(ttl);

        *guard = Some((token_resp.access_token.clone(), expiry));
        Ok(token_resp.access_token)
    }
}

#[cfg(test)]
mod tests {
    use super::pem_to_der;

    #[test]
    fn pem_to_der_rejects_pkcs1() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nAQID\n-----END RSA PRIVATE KEY-----\n";
        assert!(pem_to_der(pem).is_err());
    }

    #[test]
    fn pem_to_der_decodes_pkcs8() {
        // Minimal placeholder — just checks the stripping logic, not actual RSA validity.
        let pem = "-----BEGIN PRIVATE KEY-----\nAQID\n-----END PRIVATE KEY-----\n";
        let der = pem_to_der(pem).expect("should decode");
        assert_eq!(der, vec![0x01, 0x02, 0x03]);
    }
}

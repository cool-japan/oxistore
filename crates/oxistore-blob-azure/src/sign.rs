//! Azure Storage Shared Key v2 authentication signing.
//!
//! Reference: <https://docs.microsoft.com/en-us/rest/api/storageservices/authorize-with-shared-key>

use crate::error::AzureError;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Signer that produces Azure Shared Key authorization headers.
///
/// The canonical string format is defined in Azure Storage REST API docs
/// (Shared Key Lite variant 2).
#[derive(Clone)]
pub struct SharedKeySigner {
    account_name: String,
    key_bytes: Vec<u8>,
}

impl SharedKeySigner {
    /// Create a new signer from the account name and decoded key bytes.
    pub fn new(account_name: String, key_bytes: Vec<u8>) -> Self {
        Self {
            account_name,
            key_bytes,
        }
    }

    /// Build the complete `Authorization` header value for a request.
    ///
    /// # Parameters
    ///
    /// - `method`: HTTP verb ("GET", "PUT", "DELETE", "HEAD")
    /// - `url`: full URL (used to extract path and query string)
    /// - `headers`: all headers that will be sent (including `x-ms-*` and Content-*)
    /// - `content_length`: body size in bytes; `None` means no body (or zero, mapped to empty)
    /// - `content_type`: value of the `Content-Type` header, or `None`
    pub fn sign(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        content_length: Option<u64>,
        content_type: Option<&str>,
    ) -> Result<String, AzureError> {
        let canonicalized_headers = self.canonicalize_headers(headers);
        let canonicalized_resource = self.canonicalize_resource(url)?;

        // Per Azure docs (Shared Key for Blob, Queue, File services):
        //   VERB\nContent-Encoding\nContent-Language\nContent-Length\n
        //   Content-MD5\nContent-Type\nDate\nIf-Modified-Since\nIf-Match\n
        //   If-None-Match\nIf-Unmodified-Since\nRange\n
        //   CanonicalizedHeaders\nCanonicalizedResource
        //
        // Content-Length: empty string when 0 (per Azure spec 2015-02-21+)
        let cl_str = match content_length {
            Some(0) | None => String::new(),
            Some(n) => n.to_string(),
        };
        let ct_str = content_type.unwrap_or("");

        let string_to_sign = format!(
            "{method}\n\n\n{cl_str}\n\n{ct_str}\n\n\n\n\n\n\n{canonicalized_headers}{canonicalized_resource}"
        );

        let signature = self.hmac_sign(string_to_sign.as_bytes())?;
        Ok(format!("SharedKey {}:{}", self.account_name, signature))
    }

    /// Compute HMAC-SHA256 of `data` using the stored key bytes.
    fn hmac_sign(&self, data: &[u8]) -> Result<String, AzureError> {
        let mut mac = HmacSha256::new_from_slice(&self.key_bytes)
            .map_err(|e| AzureError::Auth(format!("HMAC key error: {e}")))?;
        mac.update(data);
        let result = mac.finalize().into_bytes();
        Ok(base64::engine::general_purpose::STANDARD.encode(result))
    }

    /// Build the canonicalized headers string from a header slice.
    ///
    /// Only `x-ms-*` headers are included.  They are sorted by lowercase name
    /// and formatted as `<lowercase-name>:<value>\n`.
    pub(crate) fn canonicalize_headers(&self, headers: &[(&str, &str)]) -> String {
        let mut ms_headers: Vec<(String, String)> = headers
            .iter()
            .filter(|(k, _)| k.to_ascii_lowercase().starts_with("x-ms-"))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
            .collect();

        // Sort by header name.
        ms_headers.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = String::new();
        for (k, v) in &ms_headers {
            out.push_str(k);
            out.push(':');
            out.push_str(v);
            out.push('\n');
        }
        out
    }

    /// Build the canonicalized resource string from a URL.
    ///
    /// Format: `/<account-name><path>` followed by sorted query params.
    pub(crate) fn canonicalize_resource(&self, url: &str) -> Result<String, AzureError> {
        // Use url crate to parse the URL reliably.
        let parsed = url::Url::parse(url)
            .map_err(|e| AzureError::Response(format!("invalid URL '{url}': {e}")))?;

        // Path portion: e.g. "/container/blob" or "/"
        let path = parsed.path();

        let mut resource = format!("/{}{}", self.account_name, path);

        // Query parameters sorted by name.
        let mut params: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
            .collect();
        params.sort_by(|a, b| a.0.cmp(&b.0));

        for (k, v) in &params {
            resource.push('\n');
            resource.push_str(k);
            resource.push(':');
            resource.push_str(v);
        }

        Ok(resource)
    }
}

// ── RFC1123 date formatting ────────────────────────────────────────────────────

/// Day-of-week names (ISO weekday 0=Mon, 6=Sun mapped to calendar Sun=0).
static DOW: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
/// Month abbreviations (1-based index).
static MON: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Format the current UTC time as an RFC1123 string suitable for the
/// `x-ms-date` header: `Mon, 27 May 2026 12:00:00 GMT`.
pub fn rfc1123_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    rfc1123_from_secs(secs)
}

/// Convert epoch seconds to RFC1123 format.
pub(crate) fn rfc1123_from_secs(secs: u64) -> String {
    // Day-of-week: Jan 1 1970 was a Thursday (4 in 0=Sun scale)
    let dow = ((secs / 86400 + 4) % 7) as usize;

    // Compute calendar date via a simple Gregorian algorithm.
    let (year, month, day, hour, min, sec) = epoch_to_calendar(secs);

    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DOW[dow],
        day,
        MON[month as usize - 1],
        year,
        hour,
        min,
        sec
    )
}

/// Convert Unix epoch seconds to (year, month[1-12], day[1-31], hour, min, sec).
fn epoch_to_calendar(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = secs % 60;
    let mins = secs / 60;
    let min = mins % 60;
    let hours = mins / 60;
    let hour = hours % 24;
    let days = hours / 24; // days since epoch

    // Shift to the March-based Gregorian calendar for easier leap-year math.
    // Civil calendar: epoch = March 1, year 1 in the "proleptic" system.
    // We use the algorithm from:
    //   https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // year of era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month of year [0, 11] (March=0)
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    (y, m, d, hour, min, sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_to_calendar_known_date() {
        // 2026-05-27 00:00:00 UTC
        // Verified: python3 -c "import datetime; d = datetime.datetime(2026,5,27,0,0,0,
        //   tzinfo=datetime.timezone.utc); print(int(d.timestamp()))"
        // => 1779840000
        let secs = 1_779_840_000u64;
        let (y, m, d, h, mi, s) = epoch_to_calendar(secs);
        assert_eq!(y, 2026);
        assert_eq!(m, 5);
        assert_eq!(d, 27);
        assert_eq!(h, 0);
        assert_eq!(mi, 0);
        assert_eq!(s, 0);
    }

    #[test]
    fn rfc1123_known_timestamp() {
        let s = rfc1123_from_secs(1_779_840_000);
        assert_eq!(s, "Wed, 27 May 2026 00:00:00 GMT");
    }

    #[test]
    fn canonicalize_headers_sorts_and_formats() {
        let signer = SharedKeySigner::new("account".to_string(), vec![0u8; 32]);
        let headers = &[
            ("x-ms-version", "2024-08-04"),
            ("x-ms-date", "Wed, 27 May 2026 00:00:00 GMT"),
            ("x-ms-blob-type", "BlockBlob"),
            ("Content-Type", "application/octet-stream"),
        ];
        let canon = signer.canonicalize_headers(headers);
        // Should only include x-ms-* in sorted order.
        let lines: Vec<&str> = canon.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("x-ms-blob-type:"));
        assert!(lines[1].starts_with("x-ms-date:"));
        assert!(lines[2].starts_with("x-ms-version:"));
    }

    #[test]
    fn canonicalize_resource_path_and_query() {
        let signer = SharedKeySigner::new("myaccount".to_string(), vec![0u8; 32]);
        let resource = signer
            .canonicalize_resource("https://myaccount.blob.core.windows.net/mycontainer?restype=container&comp=list&prefix=foo")
            .unwrap();
        assert!(resource.starts_with("/myaccount/mycontainer"));
        assert!(resource.contains("\ncomp:list"));
        assert!(resource.contains("\nprefix:foo"));
        assert!(resource.contains("\nrestype:container"));
    }
}

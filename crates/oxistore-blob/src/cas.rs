//! Content-addressed storage primitives.
//!
//! Provides the [`Digest`] type (a 32-byte SHA-256 content address), as well
//! as [`sha256`] and [`sha256_streaming`] helper functions for computing
//! SHA-256 hashes.

use std::io::Read;

use sha2::{Digest as _, Sha256};

use crate::error::BlobError;

// ── Digest type ───────────────────────────────────────────────────────────────

/// A 32-byte SHA-256 digest used as a content address.
///
/// Two blobs with the same content will always produce the same `Digest`.
/// This property enables deduplication and read-time integrity verification.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Construct a `Digest` from a raw 32-byte array.
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Return a reference to the underlying 32-byte array.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Encode the digest as a lower-case hex string (64 characters).
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in &self.0 {
            use std::fmt::Write as _;
            // Cannot fail writing to a String.
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Decode a lower-case hex string into a `Digest`.
    ///
    /// Returns [`BlobError::Other`] if the string is not a valid 64-character
    /// lowercase hex digest.
    pub fn from_hex(s: &str) -> Result<Self, BlobError> {
        if s.len() != 64 {
            return Err(BlobError::Other(format!(
                "invalid digest hex length: expected 64, got {}",
                s.len()
            )));
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0]).ok_or_else(|| {
                BlobError::Other(format!("invalid hex character '{}'", chunk[0] as char))
            })?;
            let lo = hex_nibble(chunk[1]).ok_or_else(|| {
                BlobError::Other(format!("invalid hex character '{}'", chunk[1] as char))
            })?;
            bytes[i] = (hi << 4) | lo;
        }
        Ok(Self(bytes))
    }
}

impl std::fmt::Debug for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

// ── Hash helpers ──────────────────────────────────────────────────────────────

/// Compute the SHA-256 digest of `data` in one shot.
pub fn sha256(data: &[u8]) -> Digest {
    let hash = Sha256::digest(data);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&hash);
    Digest(bytes)
}

/// Compute the SHA-256 digest of a byte stream, reading until EOF.
///
/// Returns [`BlobError::Io`] on any I/O error.
pub fn sha256_streaming<R: Read>(mut reader: R) -> Result<Digest, BlobError> {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hash = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&hash);
    Ok(Digest(bytes))
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Decode a single ASCII hex nibble into its numeric value (0..=15).
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of the empty string — NIST FIPS 180-4 known-answer test.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn sha256_empty() {
        let d = sha256(b"");
        assert_eq!(d.to_hex(), EMPTY_SHA256);
    }

    #[test]
    fn sha256_abc() {
        let d = sha256(b"abc");
        // NIST FIPS 180-4 test vector for "abc".
        assert_eq!(
            d.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn digest_hex_round_trip() {
        let original = sha256(b"hello, world");
        let hex = original.to_hex();
        let restored = Digest::from_hex(&hex).expect("from_hex");
        assert_eq!(original, restored);
    }

    #[test]
    fn digest_from_hex_bad_length() {
        let err = Digest::from_hex("deadbeef").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid digest hex length"), "{msg}");
    }

    #[test]
    fn digest_from_hex_bad_char() {
        // 64-char string with a non-hex character at position 0.
        let bad = "gg7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(bad.len(), 64, "test string must be 64 chars");
        let err = Digest::from_hex(bad).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid hex character"), "{msg}");
    }

    #[test]
    fn streaming_matches_oneshot() {
        let data = b"streaming test data";
        let oneshot = sha256(data);
        let streamed = sha256_streaming(std::io::Cursor::new(data)).expect("streaming");
        assert_eq!(oneshot, streamed);
    }
}

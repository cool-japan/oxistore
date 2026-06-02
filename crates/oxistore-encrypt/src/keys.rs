//! Key provider trait and built-in implementations for `oxistore-encrypt`.
//!
//! # Key Provider trait
//!
//! [`KeyProvider`] is a fallible source of a 32-byte XChaCha20-Poly1305 key.
//! Callers receive a reference valid for the lifetime of `&self`; all
//! implementations must hold the raw key bytes in memory.
//!
//! # Built-in implementations
//!
//! | Type | Status |
//! |------|--------|
//! | [`StaticKey`] | In-memory `Vec<u8>`; suitable for tests and simple deployments. |
//! | [`KeyringKey`] | Stub — real OS keyring wiring is deferred to M6. |

use crate::error::EncryptError;

/// Fallible source of a 32-byte AEAD key.
///
/// Implementations **must** return exactly 32 bytes on success;
/// [`KeyProvider::get_key`] returns [`EncryptError::InvalidKeyLength`] if
/// the underlying storage holds a key of any other length.
pub trait KeyProvider: Send + Sync {
    /// Return a reference to the raw key bytes (must be exactly 32 bytes).
    ///
    /// # Errors
    ///
    /// * [`EncryptError::InvalidKeyLength`] — the key is not exactly 32 bytes.
    /// * [`EncryptError::KeyringUnavailable`] — the provider cannot retrieve the
    ///   key at this time (e.g. OS keyring is unavailable).
    fn get_key(&self) -> Result<&[u8], EncryptError>;

    /// Validate that `get_key` returns exactly 32 bytes and cast the result.
    ///
    /// This is a convenience helper used internally by `encrypt_cell` and
    /// `decrypt_cell`.
    fn key32(&self) -> Result<&[u8; 32], EncryptError> {
        let raw = self.get_key()?;
        raw.try_into()
            .map_err(|_| EncryptError::InvalidKeyLength { got: raw.len() })
    }
}

// ── StaticKey ─────────────────────────────────────────────────────────────────

/// An in-memory key provider wrapping a `Vec<u8>`.
///
/// Useful for tests and deployments where the key is loaded from an
/// environment variable or configuration file.  Production code should
/// prefer [`KeyringKey`] once M6 wiring is complete.
#[derive(Clone)]
pub struct StaticKey(Vec<u8>);

impl StaticKey {
    /// Create a new [`StaticKey`] from raw bytes.
    ///
    /// `bytes` should be exactly 32 bytes for XChaCha20-Poly1305; the length
    /// is validated lazily at [`KeyProvider::get_key`] time.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Create a [`StaticKey`] from a 32-byte array (infallible at call-site).
    pub fn from_array(key: [u8; 32]) -> Self {
        Self(key.to_vec())
    }
}

impl core::fmt::Debug for StaticKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never expose key material in debug output.
        f.debug_struct("StaticKey")
            .field("len", &self.0.len())
            .finish()
    }
}

impl KeyProvider for StaticKey {
    fn get_key(&self) -> Result<&[u8], EncryptError> {
        if self.0.len() == 32 {
            Ok(&self.0)
        } else {
            Err(EncryptError::InvalidKeyLength { got: self.0.len() })
        }
    }
}

// ── KeyringKey ────────────────────────────────────────────────────────────────

/// A key provider backed by the OS keyring (stub — M6 wiring pending).
///
/// The `label` identifies the key ring entry that will be consulted in M6.
/// In the current stub implementation all calls to [`KeyProvider::get_key`]
/// return [`EncryptError::KeyringUnavailable`].
#[derive(Debug, Clone)]
pub struct KeyringKey {
    label: String,
}

impl KeyringKey {
    /// Create a new [`KeyringKey`] stub with the given `label`.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }

    /// Return the label identifying the OS keyring entry.
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl KeyProvider for KeyringKey {
    fn get_key(&self) -> Result<&[u8], EncryptError> {
        Err(EncryptError::KeyringUnavailable {
            label: self.label.clone(),
        })
    }
}

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
//! | [`KeyringKey`] | OS keyring backed: macOS Keychain, Linux secret-service, Windows Credential Manager. Enable with `os-keyring` feature for real retrieval; stub otherwise. |

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
/// prefer [`KeyringKey`] (with the `os-keyring` feature enabled) for
/// deployments that can use the platform OS keyring.
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

/// A key provider backed by the OS keyring.
///
/// When the `os-keyring` feature is **enabled**, `KeyringKey::get_key` queries
/// the OS credential store (macOS Keychain, Linux secret-service via D-Bus,
/// Windows Credential Manager) using the `keyring` crate (v4).
///
/// The stored value must be a hex-encoded 32-byte key (64 hex characters).
/// This encoding is chosen because OS keyrings typically store UTF-8 text
/// secrets; raw binary secrets can be stored as hex without ambiguity.
///
/// When the `os-keyring` feature is **disabled** the implementation falls back
/// to the original stub behaviour and always returns
/// [`EncryptError::KeyringUnavailable`].
///
/// # Storing a key
///
/// Use the `keyring` CLI or the `keyring::Entry` API to store a key:
///
/// ```sh
/// # Generate a random 32-byte key as hex and store it in the keyring:
/// key=$(openssl rand -hex 32)
/// # Store via keyring CLI (or use the keyring crate Entry API directly):
/// echo "$key" | secret-tool store --label="oxistore: my-app-enc-key" service oxistore username my-app-enc-key
/// ```
///
/// # Security
///
/// The loaded key bytes are cached in a `Vec<u8>` on first access and zeroed
/// on drop.  The raw key is **never** stored in debug output.
pub struct KeyringKey {
    label: String,
    // Loaded from OS keyring on first access; cached thereafter.
    // Protected by a std::sync::OnceLock so concurrent get_key() calls do not
    // race.  The Vec is zeroed on drop.
    #[cfg(feature = "os-keyring")]
    cached: std::sync::OnceLock<Vec<u8>>,
}

impl core::fmt::Debug for KeyringKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never expose key material.
        f.debug_struct("KeyringKey")
            .field("label", &self.label)
            .field("key_material", &"[REDACTED]")
            .finish()
    }
}

impl Clone for KeyringKey {
    fn clone(&self) -> Self {
        // Clone creates a fresh instance that will re-fetch from the OS
        // keyring on next get_key() call — we do not copy cached key bytes.
        Self::new(self.label.clone())
    }
}

impl KeyringKey {
    /// Create a new [`KeyringKey`] that will retrieve the encryption key for
    /// `label` from the OS keyring when first accessed.
    ///
    /// The `label` corresponds to the `username` field of the keyring entry.
    /// The `service` name used when querying the keyring is `"oxistore"`.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            #[cfg(feature = "os-keyring")]
            cached: std::sync::OnceLock::new(),
        }
    }

    /// Return the label identifying the OS keyring entry.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Store a 32-byte `key` in the OS keyring under this label.
    ///
    /// The key is hex-encoded before storage because OS keyrings typically
    /// expect UTF-8 text secrets.
    ///
    /// Only available when the `os-keyring` feature is enabled.
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::KeyringUnavailable`] if the keyring entry
    /// cannot be created or the OS keyring service is not running.
    #[cfg(feature = "os-keyring")]
    pub fn store_key(&self, key: &[u8; 32]) -> Result<(), EncryptError> {
        let hex = encode_hex(key);
        let entry = keyring_core::Entry::new("oxistore", &self.label).map_err(|e| {
            EncryptError::KeyringUnavailable {
                label: format!("{}: {}", self.label, e),
            }
        })?;
        entry
            .set_password(&hex)
            .map_err(|e| EncryptError::KeyringUnavailable {
                label: format!("{}: {}", self.label, e),
            })
    }

    /// Delete the OS keyring entry for this label.
    ///
    /// Only available when the `os-keyring` feature is enabled.  No-op if the
    /// entry does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::KeyringUnavailable`] if the OS keyring service
    /// is inaccessible (not if the entry is simply missing).
    #[cfg(feature = "os-keyring")]
    pub fn delete_entry(&self) -> Result<(), EncryptError> {
        let entry = keyring_core::Entry::new("oxistore", &self.label).map_err(|e| {
            EncryptError::KeyringUnavailable {
                label: format!("{}: {}", self.label, e),
            }
        })?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring_core::Error::NoEntry) => Ok(()), // already absent
            Err(e) => Err(EncryptError::KeyringUnavailable {
                label: format!("{}: {}", self.label, e),
            }),
        }
    }
}

// ── Hex helpers ───────────────────────────────────────────────────────────────

/// Encode `bytes` as lowercase hex.
#[cfg(feature = "os-keyring")]
fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Decode a hex string of exactly 64 characters into a `[u8; 32]`.
#[cfg(feature = "os-keyring")]
fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

#[cfg(feature = "os-keyring")]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ── KeyProvider implementation ────────────────────────────────────────────────

impl KeyProvider for KeyringKey {
    fn get_key(&self) -> Result<&[u8], EncryptError> {
        #[cfg(feature = "os-keyring")]
        {
            // Return cached bytes if already loaded.
            if let Some(bytes) = self.cached.get() {
                return Ok(bytes.as_slice());
            }

            // Query the OS keyring.
            let entry = keyring_core::Entry::new("oxistore", &self.label).map_err(|e| {
                EncryptError::KeyringUnavailable {
                    label: format!("{}: {}", self.label, e),
                }
            })?;
            let hex = entry
                .get_password()
                .map_err(|e| EncryptError::KeyringUnavailable {
                    label: format!("{}: {}", self.label, e),
                })?;

            let key32 = decode_hex32(&hex).ok_or_else(|| EncryptError::InvalidKeyLength {
                got: hex.trim().len() / 2,
            })?;

            // Cache and return.  If another thread stored a value
            // concurrently, the OnceLock returns the existing value.
            let stored = self.cached.get_or_init(|| key32.to_vec());
            Ok(stored.as_slice())
        }

        #[cfg(not(feature = "os-keyring"))]
        {
            Err(EncryptError::KeyringUnavailable {
                label: self.label.clone(),
            })
        }
    }
}

// ── Zeroize cached key on drop ────────────────────────────────────────────────

#[cfg(feature = "os-keyring")]
impl Drop for KeyringKey {
    fn drop(&mut self) {
        if let Some(bytes) = self.cached.get_mut() {
            // Zero out the key bytes in memory before deallocation.
            for b in bytes.iter_mut() {
                *b = 0;
            }
        }
    }
}

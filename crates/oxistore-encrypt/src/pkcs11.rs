//! PKCS#11 HSM-backed [`KeyProvider`] bridge (opt-in via the `oxicrypto-pkcs11`
//! feature).
//!
//! [`Pkcs11KeyProvider`] retrieves a 32-byte AEAD key from a PKCS#11 token by
//! label, using the [`oxicrypto-adapter-pkcs11`] adapter.  The retrieved key is
//! cached in memory on first access (and zeroed on drop), mirroring
//! [`KeyringKey`](crate::KeyringKey).
//!
//! # Honest limitation — extractable keys only
//!
//! The [`KeyProvider`] contract requires returning the **raw** key bytes
//! (`get_key(&self) -> Result<&[u8], _>`).  A PKCS#11 token only reveals a
//! secret key's `CKA_VALUE` when that key was created with
//! `CKA_EXTRACTABLE = true`.  Therefore this bridge works **only** with
//! extractable keys.
//!
//! A truly non-extractable HSM key (the primary security reason to use an HSM)
//! cannot satisfy the `get_key` contract, because the raw bytes never leave the
//! token.  Supporting non-extractable keys would require a different trait that
//! delegates the *AEAD operation itself* to the HSM (encrypt/decrypt on-token)
//! rather than exporting key material — that is out of scope for this bridge.
//! This is a documented, deliberate constraint, not a stub.
//!
//! [`oxicrypto-adapter-pkcs11`]: https://docs.rs/oxicrypto-adapter-pkcs11

use std::sync::{Arc, OnceLock};

use oxicrypto_adapter_pkcs11::Pkcs11Provider;

use crate::error::EncryptError;
use crate::keys::KeyProvider;

/// Validate that a raw key retrieved from a PKCS#11 token is exactly 32 bytes.
///
/// Returns the key as a fixed-size array, or [`EncryptError::InvalidKeyLength`]
/// if the token returned a key of any other length.
///
/// This helper contains no HSM interaction, so it is unit-testable without a
/// token.
pub fn validate_pkcs11_key(raw: Vec<u8>) -> Result<[u8; 32], EncryptError> {
    let got = raw.len();
    raw.try_into()
        .map_err(|_| EncryptError::InvalidKeyLength { got })
}

/// A [`KeyProvider`] backed by an extractable PKCS#11 secret key.
///
/// On first [`get_key`](KeyProvider::get_key) call the provider:
///
/// 1. Locates the secret key on the token by its `CKA_LABEL` (`find_secret_key`).
/// 2. Reads its `CKA_VALUE` (`extract_key_value`) — requires `CKA_EXTRACTABLE=true`.
/// 3. Validates the length is exactly 32 bytes.
/// 4. Caches the bytes for the lifetime of this provider.
///
/// See the [module docs](self) for the extractable-key limitation.
pub struct Pkcs11KeyProvider {
    provider: Arc<Pkcs11Provider>,
    label: String,
    cached: OnceLock<Vec<u8>>,
}

impl Pkcs11KeyProvider {
    /// Create a bridge that will retrieve the extractable secret key labelled
    /// `label` from `provider` on first access.
    pub fn new(provider: Arc<Pkcs11Provider>, label: impl Into<String>) -> Self {
        Self {
            provider,
            label: label.into(),
            cached: OnceLock::new(),
        }
    }

    /// The `CKA_LABEL` this provider looks up on the token.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Load and validate the key from the token (uncached path).
    fn load(&self) -> Result<[u8; 32], EncryptError> {
        let handle = self
            .provider
            .find_secret_key(&self.label)
            .map_err(|e| EncryptError::KeyProviderFailed(e.to_string()))?;
        let raw = self
            .provider
            .extract_key_value(handle)
            .map_err(|e| EncryptError::KeyProviderFailed(e.to_string()))?;
        validate_pkcs11_key(raw)
    }
}

impl core::fmt::Debug for Pkcs11KeyProvider {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never expose key material.
        f.debug_struct("Pkcs11KeyProvider")
            .field("label", &self.label)
            .field("key_material", &"[REDACTED]")
            .finish()
    }
}

impl KeyProvider for Pkcs11KeyProvider {
    fn get_key(&self) -> Result<&[u8], EncryptError> {
        if let Some(bytes) = self.cached.get() {
            return Ok(bytes.as_slice());
        }
        // Fallible load completes before we populate the cache.
        let key = self.load()?;
        let stored = self.cached.get_or_init(|| key.to_vec());
        Ok(stored.as_slice())
    }
}

// Zero the cached key bytes on drop.
impl Drop for Pkcs11KeyProvider {
    fn drop(&mut self) {
        if let Some(bytes) = self.cached.get_mut() {
            for b in bytes.iter_mut() {
                *b = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_32_bytes() {
        let key = validate_pkcs11_key(vec![7u8; 32]).expect("32 bytes is valid");
        assert_eq!(key, [7u8; 32]);
    }

    #[test]
    fn validate_rejects_short_key() {
        match validate_pkcs11_key(vec![0u8; 16]) {
            Err(EncryptError::InvalidKeyLength { got }) => assert_eq!(got, 16),
            other => panic!("expected InvalidKeyLength, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_long_key() {
        match validate_pkcs11_key(vec![0u8; 48]) {
            Err(EncryptError::InvalidKeyLength { got }) => assert_eq!(got, 48),
            other => panic!("expected InvalidKeyLength, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_empty_key() {
        match validate_pkcs11_key(Vec::new()) {
            Err(EncryptError::InvalidKeyLength { got }) => assert_eq!(got, 0),
            other => panic!("expected InvalidKeyLength, got {other:?}"),
        }
    }
}

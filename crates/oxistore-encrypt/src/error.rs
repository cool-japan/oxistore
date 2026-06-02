//! Error types for `oxistore-encrypt`.

use oxistore_core::StoreError;

/// Errors that can occur during cell encryption or decryption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptError {
    /// The key is not exactly 32 bytes.
    ///
    /// `got` is the actual key length in bytes.
    InvalidKeyLength {
        /// Actual key length received.
        got: usize,
    },

    /// The OS keyring is unavailable (stub — M6).
    ///
    /// `label` is the keyring entry label that was requested.
    KeyringUnavailable {
        /// The keyring entry label that was requested.
        label: String,
    },

    /// The ciphertext is too short to contain a nonce and/or tag.
    CiphertextTooShort {
        /// Minimum number of bytes expected.
        min_expected: usize,
        /// Actual number of bytes received.
        got: usize,
    },

    /// Authentication tag verification failed (tampered or corrupted data).
    AuthenticationFailed,

    /// Random nonce generation failed.
    RngFailed,

    /// An underlying store error propagated from the inner KV store.
    Store(String),

    // ── Envelope / Keyring errors ─────────────────────────────────────────────
    /// Key derivation failed (Argon2id or PBKDF2 error).
    KeyDerivationFailed(String),

    /// The keyring has no entry for the requested KEK version.
    MissingKekVersion(u32),

    /// AEAD encryption failed during envelope sealing.
    EncryptionFailed(String),

    /// An internal RwLock was poisoned (a thread panicked while holding it).
    LockPoisoned,

    /// The keyring contains no KEK versions (invariant violation).
    KeyringEmpty,

    /// Key rotation operation failed.
    KeyRotation {
        /// KEK version being rotated from.
        old_version: u32,
        /// KEK version being rotated to.
        new_version: u32,
        /// Reason for the failure.
        reason: String,
    },
}

impl core::fmt::Display for EncryptError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EncryptError::InvalidKeyLength { got } => {
                write!(f, "invalid key length: expected 32 bytes, got {got}")
            }
            EncryptError::KeyringUnavailable { label } => {
                write!(f, "OS keyring unavailable for label '{label}' (M6 stub)")
            }
            EncryptError::CiphertextTooShort { min_expected, got } => write!(
                f,
                "ciphertext too short: expected at least {min_expected} bytes, got {got}"
            ),
            EncryptError::AuthenticationFailed => {
                write!(f, "AEAD authentication tag verification failed")
            }
            EncryptError::RngFailed => write!(f, "CSPRNG initialization or fill failed"),
            EncryptError::Store(msg) => write!(f, "underlying store error: {msg}"),
            EncryptError::KeyDerivationFailed(msg) => {
                write!(f, "key derivation failed: {msg}")
            }
            EncryptError::MissingKekVersion(v) => {
                write!(f, "keyring has no entry for KEK version {v}")
            }
            EncryptError::EncryptionFailed(msg) => {
                write!(f, "AEAD encryption failed: {msg}")
            }
            EncryptError::LockPoisoned => {
                write!(
                    f,
                    "internal RwLock was poisoned (a thread panicked while holding it)"
                )
            }
            EncryptError::KeyringEmpty => {
                write!(f, "keyring contains no KEK versions (invariant violation)")
            }
            EncryptError::KeyRotation {
                old_version,
                new_version,
                reason,
            } => {
                write!(
                    f,
                    "key rotation from version {old_version} to {new_version} failed: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for EncryptError {}

impl From<StoreError> for EncryptError {
    fn from(e: StoreError) -> Self {
        EncryptError::Store(e.to_string())
    }
}

impl From<EncryptError> for StoreError {
    fn from(e: EncryptError) -> Self {
        StoreError::Other(e.to_string())
    }
}

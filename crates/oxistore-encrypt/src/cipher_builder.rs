//! Builder API for constructing [`EncryptedKv`] with different AEAD choices
//! and key sources.
//!
//! # Example
//!
//! ```no_run
//! use oxistore_encrypt::{CipherBuilder, AeadChoice};
//! // let store = redb_store; // any T: KvStore
//! // let enc = CipherBuilder::new()
//! //     .aead(AeadChoice::AesGcmSiv256)
//! //     .key([0x42u8; 32])
//! //     .build(store)
//! //     .expect("build failed");
//! ```

use oxicrypto::{argon2id_derive, Argon2Params};

use oxistore_core::KvStore;

use crate::aead::AeadKind;
use crate::decorator::EncryptedKv;
use crate::error::EncryptError;
use crate::keys::StaticKey;

// ── AeadChoice ────────────────────────────────────────────────────────────────

/// AEAD algorithm selection for [`CipherBuilder`].
#[derive(Debug, Clone, Copy, Default)]
pub enum AeadChoice {
    /// XChaCha20-Poly1305 (24-byte nonce). Default.
    #[default]
    XChaCha20Poly1305,
    /// AES-256-GCM-SIV (12-byte nonce, misuse-resistant).
    AesGcmSiv256,
}

// ── KeySource ─────────────────────────────────────────────────────────────────

/// Source of the 32-byte encryption key.
#[derive(Debug)]
pub enum KeySource {
    /// A pre-computed raw 32-byte key.
    Raw([u8; 32]),
    /// Derive a key from a passphrase and salt using Argon2id.
    Passphrase {
        /// The passphrase bytes.
        passphrase: Vec<u8>,
        /// A 32-byte random salt (use [`crate::generate_salt`] to produce).
        salt: [u8; 32],
    },
}

// ── CipherBuilder ─────────────────────────────────────────────────────────────

/// Fluent builder for [`EncryptedKv`].
///
/// Allows selecting an AEAD algorithm and key source before wrapping a
/// [`KvStore`].
#[derive(Debug, Default)]
pub struct CipherBuilder {
    aead: AeadChoice,
    key_source: Option<KeySource>,
}

impl CipherBuilder {
    /// Create a new builder with defaults (XChaCha20-Poly1305, no key set).
    pub fn new() -> Self {
        Self::default()
    }

    /// Select the AEAD algorithm.
    pub fn aead(mut self, a: AeadChoice) -> Self {
        self.aead = a;
        self
    }

    /// Use the given raw 32-byte key.
    pub fn key(mut self, k: [u8; 32]) -> Self {
        self.key_source = Some(KeySource::Raw(k));
        self
    }

    /// Derive the key from a passphrase and random salt using Argon2id
    /// (production parameters: m=65536, t=3, p=1).
    pub fn passphrase(mut self, p: impl Into<Vec<u8>>, salt: [u8; 32]) -> Self {
        self.key_source = Some(KeySource::Passphrase {
            passphrase: p.into(),
            salt,
        });
        self
    }

    /// Build an [`EncryptedKv`] wrapping `store` with the configured cipher.
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::InvalidKeyLength`] if no key was configured, or
    /// [`EncryptError::KeyDerivationFailed`] if Argon2id derivation fails.
    pub fn build<S: KvStore>(
        self,
        store: S,
    ) -> Result<EncryptedKv<S, StaticKey, AeadKind>, EncryptError> {
        let raw_key = self.resolve_key(false)?;
        let key_provider = StaticKey::from_array(raw_key);
        let kind = match self.aead {
            AeadChoice::XChaCha20Poly1305 => AeadKind::XChaCha20Poly1305,
            AeadChoice::AesGcmSiv256 => AeadKind::AesGcmSiv256,
        };
        Ok(EncryptedKv::with_aead(store, key_provider, kind))
    }

    /// Build using fast (test) Argon2id params for passphrase-based keys.
    ///
    /// Has no effect if a raw key was supplied.
    pub fn build_test<S: KvStore>(
        self,
        store: S,
    ) -> Result<EncryptedKv<S, StaticKey, AeadKind>, EncryptError> {
        let raw_key = self.resolve_key(true)?;
        let key_provider = StaticKey::from_array(raw_key);
        let kind = match self.aead {
            AeadChoice::XChaCha20Poly1305 => AeadKind::XChaCha20Poly1305,
            AeadChoice::AesGcmSiv256 => AeadKind::AesGcmSiv256,
        };
        Ok(EncryptedKv::with_aead(store, key_provider, kind))
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn resolve_key(&self, use_test_params: bool) -> Result<[u8; 32], EncryptError> {
        match &self.key_source {
            None => Err(EncryptError::InvalidKeyLength { got: 0 }),
            Some(KeySource::Raw(k)) => Ok(*k),
            Some(KeySource::Passphrase { passphrase, salt }) => {
                let params = if use_test_params {
                    Argon2Params::TEST_PARAMS
                } else {
                    Argon2Params {
                        m_cost: 65_536,
                        t_cost: 3,
                        p_cost: 1,
                    }
                };
                let mut key = [0u8; 32];
                argon2id_derive(passphrase, salt.as_ref(), params, &mut key)
                    .map_err(|e| EncryptError::KeyDerivationFailed(e.to_string()))?;
                Ok(key)
            }
        }
    }
}

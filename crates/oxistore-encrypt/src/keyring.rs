//! Versioned Key-Encrypting Key (KEK) ring for envelope encryption.
//!
//! # Overview
//!
//! The [`Keyring`] holds a sequence of versioned KEKs.  The last entry is
//! always the *active* KEK used to wrap new DEKs; all previous entries are
//! retained so that ciphertexts produced under older KEKs can still be
//! unwrapped.
//!
//! # Passphrase derivation
//!
//! [`Keyring::from_passphrase`] uses **Argon2id** (RFC 9106) to derive a
//! 32-byte KEK from a user passphrase and a random 32-byte salt:
//!
//! - `m_cost = 65_536` KiB (64 MiB)
//! - `t_cost = 3` passes
//! - `p_cost = 1` lane
//!
//! For tests, use [`Keyring::from_passphrase_with_params`] with
//! [`oxicrypto::Argon2Params::TEST_PARAMS`] to keep suites fast.

use oxicrypto::{argon2id_derive, new_rng, Argon2Params};

use crate::error::EncryptError;

// ── KeyVersion ────────────────────────────────────────────────────────────────

/// A single versioned Key-Encrypting Key.
#[derive(Clone)]
pub struct KeyVersion {
    /// Monotonically increasing version number (starts at 1).
    pub version: u32,
    /// Raw 256-bit key material.
    pub kek: [u8; 32],
}

impl core::fmt::Debug for KeyVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never expose key material in debug output.
        f.debug_struct("KeyVersion")
            .field("version", &self.version)
            .field("kek", &"[REDACTED]")
            .finish()
    }
}

// ── Keyring ───────────────────────────────────────────────────────────────────

/// Holds the active KEK and all previous KEK versions.
///
/// Versions are sorted in ascending order; the last element is the active KEK.
#[derive(Clone, Debug)]
pub struct Keyring {
    versions: Vec<KeyVersion>,
}

impl Keyring {
    /// Create a new keyring with a single KEK at version 1.
    pub fn new(kek: [u8; 32]) -> Self {
        Self {
            versions: vec![KeyVersion { version: 1, kek }],
        }
    }

    /// Derive a KEK from `passphrase` and `salt` using Argon2id with
    /// production parameters (`m=65536, t=3, p=1`), then create a keyring.
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::KeyDerivationFailed`] if Argon2id fails.
    pub fn from_passphrase(passphrase: &[u8], salt: &[u8; 32]) -> Result<Self, EncryptError> {
        Self::from_passphrase_with_params(
            passphrase,
            salt,
            Argon2Params {
                m_cost: 65_536,
                t_cost: 3,
                p_cost: 1,
            },
        )
    }

    /// Derive a KEK from `passphrase` and `salt` using Argon2id with
    /// caller-supplied `params`.
    ///
    /// Prefer [`Keyring::from_passphrase`] for production use.
    /// In tests, pass [`oxicrypto::Argon2Params::TEST_PARAMS`] to keep
    /// derivation fast.
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::KeyDerivationFailed`] if Argon2id fails.
    pub fn from_passphrase_with_params(
        passphrase: &[u8],
        salt: &[u8; 32],
        params: Argon2Params,
    ) -> Result<Self, EncryptError> {
        let mut kek = [0u8; 32];
        argon2id_derive(passphrase, salt.as_ref(), params, &mut kek)
            .map_err(|e| EncryptError::KeyDerivationFailed(e.to_string()))?;
        Ok(Self::new(kek))
    }

    /// Return the version number of the currently active KEK.
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::KeyringEmpty`] if the keyring has no KEK versions
    /// (invariant violation — should never occur with the public constructors).
    pub fn active_version(&self) -> Result<u32, EncryptError> {
        self.versions
            .last()
            .map(|kv| kv.version)
            .ok_or(EncryptError::KeyringEmpty)
    }

    /// Return a reference to the active KEK bytes.
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::KeyringEmpty`] if the keyring has no KEK versions
    /// (invariant violation — should never occur with the public constructors).
    pub fn active_kek(&self) -> Result<&[u8; 32], EncryptError> {
        self.versions
            .last()
            .map(|kv| &kv.kek)
            .ok_or(EncryptError::KeyringEmpty)
    }

    /// Look up the KEK for the given `version`, or `None` if not present.
    pub fn kek_for_version(&self, version: u32) -> Option<&[u8; 32]> {
        self.versions
            .iter()
            .find(|kv| kv.version == version)
            .map(|kv| &kv.kek)
    }

    /// Add `new_kek` as the next version and make it active.
    ///
    /// The new version number is `active_version + 1`.
    /// Returns the new version number.
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::KeyringEmpty`] if the keyring has no existing
    /// versions (invariant violation — should never occur in practice).
    pub fn rotate(&mut self, new_kek: [u8; 32]) -> Result<u32, EncryptError> {
        let next_version = self.active_version()? + 1;
        self.versions.push(KeyVersion {
            version: next_version,
            kek: new_kek,
        });
        Ok(next_version)
    }

    /// Return all version numbers held by this keyring (sorted ascending).
    pub fn version_numbers(&self) -> Vec<u32> {
        self.versions.iter().map(|kv| kv.version).collect()
    }
}

// ── RNG helpers ───────────────────────────────────────────────────────────────

/// Generate a cryptographically random 32-byte salt for passphrase derivation.
///
/// # Errors
///
/// Returns [`EncryptError::RngFailed`] if the OS CSPRNG is unavailable.
pub fn generate_salt() -> Result<[u8; 32], EncryptError> {
    let mut rng = new_rng().map_err(|_| EncryptError::RngFailed)?;
    let mut salt = [0u8; 32];
    rng.fill(&mut salt).map_err(|_| EncryptError::RngFailed)?;
    Ok(salt)
}

/// Generate a cryptographically random 32-byte Data Encryption Key.
///
/// # Errors
///
/// Returns [`EncryptError::RngFailed`] if the OS CSPRNG is unavailable.
pub fn generate_dek() -> Result<[u8; 32], EncryptError> {
    let mut rng = new_rng().map_err(|_| EncryptError::RngFailed)?;
    let mut dek = [0u8; 32];
    rng.fill(&mut dek).map_err(|_| EncryptError::RngFailed)?;
    Ok(dek)
}

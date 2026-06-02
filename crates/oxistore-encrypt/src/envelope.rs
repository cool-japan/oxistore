//! Envelope encryption for OxiStore KV values.
//!
//! # Model
//!
//! Each value is encrypted with a unique, randomly-generated **Data Encryption
//! Key (DEK)**.  The DEK itself is wrapped (encrypted) under the active
//! **Key Encryption Key (KEK)** held in a [`Keyring`].  Only the tiny DEK
//! wrapper needs to change during key rotation — the bulk data ciphertext is
//! untouched.
//!
//! # Wire format (immutable once written)
//!
//! ```text
//! ┌────────────┬──────────────┬─────────────────┬───────────────┬───────────────────────┐
//! │ kek_version│  wrap_nonce  │   wrapped_dek   │  data_nonce   │   data_ciphertext     │
//! │  4 bytes   │  24 bytes    │  48 bytes       │  24 bytes     │  plaintext_len+16 bytes│
//! │  u32-LE    │  XChaCha20   │  DEK(32)+tag(16)│  XChaCha20   │  XChaCha20-Poly1305   │
//! └────────────┴──────────────┴─────────────────┴───────────────┴───────────────────────┘
//! ```
//!
//! | Field | Offset | Size | Notes |
//! |-------|--------|------|-------|
//! | `kek_version` | 0 | 4 | u32-LE version of KEK used to wrap the DEK |
//! | `wrap_nonce` | 4 | 24 | Fresh random nonce for DEK wrapping |
//! | `wrapped_dek` | 28 | 48 | DEK (32 bytes) encrypted under KEK, plus 16-byte tag |
//! | `data_nonce` | 76 | 24 | Fresh random nonce for data encryption |
//! | `data_ct` | 100 | ≥16 | Plaintext encrypted under DEK, plus 16-byte tag |
//!
//! Minimum total envelope size: 100 + 16 = **116 bytes** (for 0-byte plaintext).
//!
//! # Algorithms
//!
//! - Both DEK wrapping and data encryption use **XChaCha20-Poly1305** (32-byte
//!   key, 24-byte nonce, 16-byte tag).
//! - Every `encrypt` call generates fresh random nonces for both layers.
//! - AAD for the DEK wrapper is `kek_version.to_le_bytes()` — binding the
//!   wrapped DEK to its version.
//! - AAD for the data layer is forwarded from the caller (e.g., a
//!   `CellId`-derived byte sequence).

use std::sync::{Arc, RwLock};

use oxicrypto::{new_rng, XChaCha20Poly1305};

use crate::error::EncryptError;
use crate::keyring::{generate_dek, Keyring};
use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};

// ── Layout constants ──────────────────────────────────────────────────────────

const KEK_VERSION_LEN: usize = 4; // u32-LE
const WRAP_NONCE_LEN: usize = 24; // XChaCha20 nonce
const WRAPPED_DEK_LEN: usize = 48; // 32-byte DEK + 16-byte tag
const DATA_NONCE_LEN: usize = 24; // XChaCha20 nonce
const DATA_TAG_LEN: usize = 16; // Poly1305 tag

const WRAP_NONCE_OFFSET: usize = KEK_VERSION_LEN;
const WRAPPED_DEK_OFFSET: usize = WRAP_NONCE_OFFSET + WRAP_NONCE_LEN;
const DATA_NONCE_OFFSET: usize = WRAPPED_DEK_OFFSET + WRAPPED_DEK_LEN;
const DATA_CT_OFFSET: usize = DATA_NONCE_OFFSET + DATA_NONCE_LEN;

/// Minimum valid envelope size (covers 0-byte plaintext).
pub const MIN_ENVELOPE_LEN: usize = DATA_CT_OFFSET + DATA_TAG_LEN;

// ── EnvelopeCipher ────────────────────────────────────────────────────────────

/// Envelope cipher that wraps each value under a unique DEK, which itself
/// is wrapped under the active KEK in the [`Keyring`].
///
/// Holds the keyring inside an `Arc<RwLock<…>>` so that the cipher can be
/// shared across threads and rotated in place without requiring the caller
/// to discard existing handles.
#[derive(Clone, Debug)]
pub struct EnvelopeCipher {
    keyring: Arc<RwLock<Keyring>>,
}

impl EnvelopeCipher {
    /// Create a new [`EnvelopeCipher`] backed by `keyring`.
    pub fn new(keyring: Keyring) -> Self {
        Self {
            keyring: Arc::new(RwLock::new(keyring)),
        }
    }

    /// Encrypt `plaintext` and return an envelope-formatted ciphertext.
    ///
    /// The caller may supply `aad` (e.g. a serialised `CellId`) to bind the
    /// ciphertext to its intended storage location; use `b""` if not needed.
    ///
    /// # Errors
    ///
    /// * [`EncryptError::RngFailed`] — OS CSPRNG unavailable.
    /// * [`EncryptError::EncryptionFailed`] — AEAD seal failed.
    pub fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, EncryptError> {
        // 1. Snapshot the active KEK under a short read lock.
        let (kek_version, kek) = {
            let ring = self
                .keyring
                .read()
                .map_err(|_| EncryptError::LockPoisoned)?;
            let v = ring.active_version()?;
            let k = *ring.active_kek()?;
            (v, k)
        };

        // 2. Generate a fresh DEK and nonces.
        let dek = generate_dek()?;
        let wrap_nonce = random_nonce_24()?;
        let data_nonce = random_nonce_24()?;

        let cipher = XChaCha20Poly1305;

        // 3. Wrap DEK under the KEK.
        //    AAD = kek_version as 4 LE bytes, binding wrap to version.
        let version_aad = kek_version.to_le_bytes();
        let mut wrapped_dek = [0u8; WRAPPED_DEK_LEN];
        cipher
            .seal(&kek, &wrap_nonce, &version_aad, &dek, &mut wrapped_dek)
            .map_err(|e| EncryptError::EncryptionFailed(e.to_string()))?;

        // 4. Encrypt plaintext under the DEK.
        let data_ct_len = plaintext.len() + DATA_TAG_LEN;
        let mut output = vec![0u8; DATA_CT_OFFSET + data_ct_len];

        output[..KEK_VERSION_LEN].copy_from_slice(&kek_version.to_le_bytes());
        output[WRAP_NONCE_OFFSET..WRAPPED_DEK_OFFSET].copy_from_slice(&wrap_nonce);
        output[WRAPPED_DEK_OFFSET..DATA_NONCE_OFFSET].copy_from_slice(&wrapped_dek);
        output[DATA_NONCE_OFFSET..DATA_CT_OFFSET].copy_from_slice(&data_nonce);

        cipher
            .seal(
                &dek,
                &data_nonce,
                aad,
                plaintext,
                &mut output[DATA_CT_OFFSET..],
            )
            .map_err(|e| EncryptError::EncryptionFailed(e.to_string()))?;

        Ok(output)
    }

    /// Decrypt an envelope ciphertext produced by [`EnvelopeCipher::encrypt`].
    ///
    /// # Errors
    ///
    /// * [`EncryptError::CiphertextTooShort`] — fewer than `MIN_ENVELOPE_LEN` bytes.
    /// * [`EncryptError::MissingKekVersion`] — keyring has no entry for the stored version.
    /// * [`EncryptError::AuthenticationFailed`] — AEAD tag verification failed.
    pub fn decrypt(&self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, EncryptError> {
        if ciphertext.len() < MIN_ENVELOPE_LEN {
            return Err(EncryptError::CiphertextTooShort {
                min_expected: MIN_ENVELOPE_LEN,
                got: ciphertext.len(),
            });
        }

        // 1. Parse header.
        let kek_version =
            u32::from_le_bytes(ciphertext[..KEK_VERSION_LEN].try_into().map_err(|_| {
                EncryptError::CiphertextTooShort {
                    min_expected: MIN_ENVELOPE_LEN,
                    got: ciphertext.len(),
                }
            })?);

        let wrap_nonce: &[u8; WRAP_NONCE_LEN] = ciphertext[WRAP_NONCE_OFFSET..WRAPPED_DEK_OFFSET]
            .try_into()
            .map_err(|_| EncryptError::CiphertextTooShort {
                min_expected: MIN_ENVELOPE_LEN,
                got: ciphertext.len(),
            })?;

        let wrapped_dek_bytes = &ciphertext[WRAPPED_DEK_OFFSET..DATA_NONCE_OFFSET];

        let data_nonce: &[u8; DATA_NONCE_LEN] = ciphertext[DATA_NONCE_OFFSET..DATA_CT_OFFSET]
            .try_into()
            .map_err(|_| EncryptError::CiphertextTooShort {
                min_expected: MIN_ENVELOPE_LEN,
                got: ciphertext.len(),
            })?;

        let data_ct = &ciphertext[DATA_CT_OFFSET..];

        // 2. Look up the KEK for the stored version.
        let kek = {
            let ring = self
                .keyring
                .read()
                .map_err(|_| EncryptError::LockPoisoned)?;
            ring.kek_for_version(kek_version)
                .copied()
                .ok_or(EncryptError::MissingKekVersion(kek_version))?
        };

        // 3. Unwrap the DEK.
        let cipher = XChaCha20Poly1305;
        let version_aad = kek_version.to_le_bytes();
        let mut dek = [0u8; 32];
        cipher
            .open(&kek, wrap_nonce, &version_aad, wrapped_dek_bytes, &mut dek)
            .map_err(|_| EncryptError::AuthenticationFailed)?;

        // 4. Decrypt the data.
        let pt_len = data_ct.len().saturating_sub(DATA_TAG_LEN);
        let mut plaintext = vec![0u8; pt_len];
        cipher
            .open(&dek, data_nonce, aad, data_ct, &mut plaintext)
            .map_err(|_| EncryptError::AuthenticationFailed)?;

        Ok(plaintext)
    }

    /// Return the currently active KEK version number.
    pub fn active_version(&self) -> Result<u32, EncryptError> {
        let ring = self
            .keyring
            .read()
            .map_err(|_| EncryptError::LockPoisoned)?;
        ring.active_version()
    }

    /// Add `new_kek` as the next KEK version without touching stored data.
    ///
    /// After this call the cipher will wrap all new DEKs under `new_kek`.
    /// Existing ciphertexts remain decryptable because the old KEK is
    /// retained in the keyring.
    ///
    /// Returns the new version number.
    pub fn add_kek_version(&self, new_kek: [u8; 32]) -> Result<u32, EncryptError> {
        let mut ring = self
            .keyring
            .write()
            .map_err(|_| EncryptError::LockPoisoned)?;
        ring.rotate(new_kek)
    }

    /// Re-wrap the DEK wrapper portion of each envelope under the new KEK.
    ///
    /// This is the cheap O(n) rotation operation: only the 48-byte DEK
    /// wrapper is re-encrypted; the data ciphertext is untouched.
    ///
    /// Returns the re-wrapped envelope bytes.
    pub(crate) fn rewrap_envelope(&self, envelope: &[u8]) -> Result<Vec<u8>, EncryptError> {
        if envelope.len() < MIN_ENVELOPE_LEN {
            return Err(EncryptError::CiphertextTooShort {
                min_expected: MIN_ENVELOPE_LEN,
                got: envelope.len(),
            });
        }

        // Parse the old KEK version and header.
        let old_version =
            u32::from_le_bytes(envelope[..KEK_VERSION_LEN].try_into().map_err(|_| {
                EncryptError::CiphertextTooShort {
                    min_expected: MIN_ENVELOPE_LEN,
                    got: envelope.len(),
                }
            })?);

        let old_wrap_nonce: &[u8; WRAP_NONCE_LEN] = envelope[WRAP_NONCE_OFFSET..WRAPPED_DEK_OFFSET]
            .try_into()
            .map_err(|_| EncryptError::CiphertextTooShort {
                min_expected: MIN_ENVELOPE_LEN,
                got: envelope.len(),
            })?;

        let old_wrapped_dek = &envelope[WRAPPED_DEK_OFFSET..DATA_NONCE_OFFSET];

        // Look up keys under a read lock.
        let (old_kek, new_kek_version, new_kek) = {
            let ring = self
                .keyring
                .read()
                .map_err(|_| EncryptError::LockPoisoned)?;
            let old_kek = ring
                .kek_for_version(old_version)
                .copied()
                .ok_or(EncryptError::MissingKekVersion(old_version))?;
            let new_v = ring.active_version()?;
            let new_kek = *ring.active_kek()?;
            (old_kek, new_v, new_kek)
        };

        let cipher = XChaCha20Poly1305;

        // Unwrap the DEK using the old KEK.
        let old_version_aad = old_version.to_le_bytes();
        let mut dek = [0u8; 32];
        cipher
            .open(
                &old_kek,
                old_wrap_nonce,
                &old_version_aad,
                old_wrapped_dek,
                &mut dek,
            )
            .map_err(|_| EncryptError::AuthenticationFailed)?;

        // Re-wrap the DEK under the new KEK with a fresh nonce.
        let new_wrap_nonce = random_nonce_24()?;
        let new_version_aad = new_kek_version.to_le_bytes();
        let mut new_wrapped_dek = [0u8; WRAPPED_DEK_LEN];
        cipher
            .seal(
                &new_kek,
                &new_wrap_nonce,
                &new_version_aad,
                &dek,
                &mut new_wrapped_dek,
            )
            .map_err(|e| EncryptError::EncryptionFailed(e.to_string()))?;

        // Reconstruct the envelope: new header + old data section.
        let mut new_envelope = Vec::with_capacity(envelope.len());
        new_envelope.extend_from_slice(&new_kek_version.to_le_bytes());
        new_envelope.extend_from_slice(&new_wrap_nonce);
        new_envelope.extend_from_slice(&new_wrapped_dek);
        // Copy data_nonce + data_ciphertext verbatim.
        new_envelope.extend_from_slice(&envelope[DATA_NONCE_OFFSET..]);

        Ok(new_envelope)
    }
}

// ── rotate_all_keys ───────────────────────────────────────────────────────────

/// Re-wrap every DEK in `store` under a new KEK without touching the data.
///
/// 1. Adds `new_kek` as the next version in `cipher`'s keyring.
/// 2. Iterates all entries in `store`.
/// 3. For each entry, re-wraps the DEK wrapper under the new KEK.
/// 4. Writes the updated envelope back to the store.
///
/// Returns the count of rotated entries.
///
/// # Errors
///
/// Returns [`EncryptError`] on RNG failure, AEAD failure, or store I/O error.
pub fn rotate_all_keys<S: KvStore>(
    store: &mut S,
    cipher: &mut EnvelopeCipher,
    new_kek: [u8; 32],
) -> Result<u64, EncryptError> {
    // Add the new KEK version to the keyring first.
    cipher.add_kek_version(new_kek)?;

    // Collect all keys and their raw (envelope) values.
    let entries: Vec<(Vec<u8>, Vec<u8>)> = store
        .iter()
        .map_err(|e| EncryptError::Store(e.to_string()))?
        .map(|item| item.map_err(|e| EncryptError::Store(e.to_string())))
        .collect::<Result<Vec<_>, _>>()?;

    let mut count: u64 = 0;
    for (key, old_envelope) in &entries {
        let new_envelope = cipher.rewrap_envelope(old_envelope)?;
        store
            .put(key, &new_envelope)
            .map_err(|e| EncryptError::Store(e.to_string()))?;
        count += 1;
    }

    Ok(count)
}

// ── EncryptedKvEnvelope ───────────────────────────────────────────────────────

/// A transparent envelope-encryption decorator wrapping any [`KvStore`].
///
/// Every value written to the inner store is encrypted with a unique random
/// DEK wrapped under the active KEK.  Key rotation re-wraps only the DEK
/// wrapper — the data ciphertext is never re-encrypted.
pub struct EncryptedKvEnvelope<S: KvStore> {
    inner: Arc<S>,
    cipher: EnvelopeCipher,
}

impl<S: KvStore> EncryptedKvEnvelope<S> {
    /// Wrap `inner` with envelope encryption using `cipher`.
    pub fn new(inner: S, cipher: EnvelopeCipher) -> Self {
        Self {
            inner: Arc::new(inner),
            cipher,
        }
    }

    /// Return a reference to the [`EnvelopeCipher`] for key rotation operations.
    pub fn cipher(&self) -> &EnvelopeCipher {
        &self.cipher
    }

    /// Rotate all DEK wrappers in the inner store to a new KEK.
    ///
    /// Returns the count of entries rotated.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Other`] wrapping an [`EncryptError`] description
    /// on failure.
    pub fn rotate_kek(&mut self, new_kek: [u8; 32]) -> Result<u64, StoreError> {
        // We need mutable access to the inner store for rotation.
        // Since inner is Arc<S> and KvStore::put takes &self, we can iterate
        // and re-put directly.
        let entries: Vec<(Vec<u8>, Vec<u8>)> = self
            .inner
            .iter()
            .map_err(|e| StoreError::Other(e.to_string()))?
            .map(|item| item.map_err(|e| StoreError::Other(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?;

        // Add the new KEK version to the keyring.
        self.cipher
            .add_kek_version(new_kek)
            .map_err(|e| StoreError::Other(e.to_string()))?;

        let mut count: u64 = 0;
        for (key, old_envelope) in &entries {
            let new_envelope = self
                .cipher
                .rewrap_envelope(old_envelope)
                .map_err(|e| StoreError::Other(e.to_string()))?;
            self.inner.put(key, &new_envelope)?;
            count += 1;
        }

        Ok(count)
    }
}

impl<S: KvStore> KvStore for EncryptedKvEnvelope<S> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        match self.inner.get(key)? {
            None => Ok(None),
            Some(ct) => {
                let pt = self.cipher.decrypt(&ct, key).map_err(StoreError::from)?;
                Ok(Some(pt))
            }
        }
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let ct = self.cipher.encrypt(value, key).map_err(StoreError::from)?;
        self.inner.put(key, &ct)
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.inner.delete(key)
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        self.inner.contains(key)
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let cipher = self.cipher.clone();
        let raw_iter = self.inner.range(lo, hi)?;

        let iter = raw_iter.map(move |item| {
            let (k, ct) = item?;
            let pt = cipher.decrypt(&ct, &k).map_err(StoreError::from)?;
            Ok((k, pt))
        });
        Ok(Box::new(iter))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Err(StoreError::Other(
            "EncryptedKvEnvelope: encrypted transactions not yet supported".to_string(),
        ))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Other(
            "EncryptedKvEnvelope: encrypted snapshots not yet supported".to_string(),
        ))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let cipher = self.cipher.clone();
        let raw_iter = self.inner.iter()?;

        let iter = raw_iter.map(move |item| {
            let (k, ct) = item?;
            let pt = cipher.decrypt(&ct, &k).map_err(StoreError::from)?;
            Ok((k, pt))
        });
        Ok(Box::new(iter))
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.inner.flush()
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn random_nonce_24() -> Result<[u8; 24], EncryptError> {
    let mut rng = new_rng().map_err(|_| EncryptError::RngFailed)?;
    let mut nonce = [0u8; 24];
    rng.fill(&mut nonce).map_err(|_| EncryptError::RngFailed)?;
    Ok(nonce)
}

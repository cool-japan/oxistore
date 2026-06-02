//! `EncryptedKv<T, K, A>` — transparent AEAD encryption decorator for [`KvStore`].
//!
//! Wrapping any [`KvStore`] implementation with [`EncryptedKv`] automatically
//! encrypts all values on write and decrypts them on read.  Keys are stored
//! in plaintext (only values are encrypted).
//!
//! # Cell identity
//!
//! The decorator derives a stable 32-byte cell identity from the raw key bytes
//! using **BLAKE3**.  The cell ID is passed as AAD (additional authenticated
//! data) on every seal/open call, binding each ciphertext to its storage
//! location.  Transplanting a ciphertext to a different key causes
//! authentication failure.  Use [`EncryptedKv::put_cell`] /
//! [`EncryptedKv::get_cell`] to supply an explicit `CellId` instead.
//!
//! # Transactions / Snapshots
//!
//! Use [`EncryptedKv::transaction`] to obtain an [`EncryptedTxn`] and
//! [`EncryptedKv::snapshot`] to obtain an [`EncryptedSnapshot`].  Both wrap
//! the inner store's transaction/snapshot with transparent encryption.
//!
//! # AEAD choice
//!
//! The third type parameter `A: Aead` selects the cipher.  It defaults to
//! [`XChaCha20Poly1305Aead`], so existing call-sites that use
//! `EncryptedKv::new(inner, key)` remain unchanged.

use std::sync::Arc;

use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};

use crate::aead::{
    decrypt_with_aead, derive_cell_id, encrypt_with_aead, AeadKind, XChaCha20Poly1305Aead,
};
use crate::cell::{decrypt_cell, encrypt_cell, CellId};
use crate::error::EncryptError;
use crate::keys::KeyProvider;
use crate::snapshot::EncryptedSnapshot;
use crate::txn::EncryptedTxn;

// ── EncryptedKv ───────────────────────────────────────────────────────────────

/// A transparent AEAD encryption decorator wrapping any [`KvStore`].
///
/// All values are encrypted before being written to the inner store and
/// decrypted transparently on read.  Keys are stored in plaintext.
///
/// # Type parameters
///
/// * `T` — the inner [`KvStore`] implementation.
/// * `K` — a [`KeyProvider`] supplying the 32-byte encryption key.
/// * `A` — the [`crate::aead::Aead`] cipher to use (default: XChaCha20-Poly1305).
pub struct EncryptedKv<T: KvStore, K: KeyProvider, A: crate::aead::Aead = XChaCha20Poly1305Aead> {
    inner: Arc<T>,
    key_provider: Arc<K>,
    aead: A,
}

impl<T: KvStore, K: KeyProvider, A: crate::aead::Aead> std::fmt::Debug for EncryptedKv<T, K, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedKv")
            .field("inner_type", &std::any::type_name::<T>())
            .field("key_material", &"[REDACTED]")
            .field("aead_type", &std::any::type_name::<A>())
            .finish()
    }
}

impl<T: KvStore, K: KeyProvider> EncryptedKv<T, K, XChaCha20Poly1305Aead> {
    /// Wrap `inner` with XChaCha20-Poly1305 encryption using `key_provider`.
    ///
    /// This is the standard constructor.  To use a different cipher, call
    /// [`EncryptedKv::with_aead`].
    pub fn new(inner: T, key_provider: K) -> Self {
        Self {
            inner: Arc::new(inner),
            key_provider: Arc::new(key_provider),
            aead: XChaCha20Poly1305Aead,
        }
    }
}

impl<T: KvStore, K: KeyProvider, A: crate::aead::Aead> EncryptedKv<T, K, A> {
    /// Wrap `inner` with `aead` encryption using `key_provider`.
    pub fn with_aead(inner: T, key_provider: K, aead: A) -> Self {
        Self {
            inner: Arc::new(inner),
            key_provider: Arc::new(key_provider),
            aead,
        }
    }

    // ── Low-level helpers ─────────────────────────────────────────────────────

    /// Encrypt `plaintext` binding it to `aad`.
    fn encrypt_value(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, EncryptError> {
        let key = self.key_provider.key32()?;
        encrypt_with_aead(&self.aead, key, aad, plaintext)
    }

    /// Decrypt ciphertext expecting `aad` to match.
    fn decrypt_value(&self, aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, EncryptError> {
        let key = self.key_provider.key32()?;
        decrypt_with_aead(&self.aead, key, aad, ciphertext)
    }

    // ── Explicit CellId variants ──────────────────────────────────────────────

    /// Encrypt `value` under an explicit [`CellId`] and store it.
    ///
    /// The `CellId` is serialised to 20 bytes and used as AAD.  This
    /// is the original per-field binding API; prefer the plain
    /// `put`/`get` methods for key-based derivation.
    pub fn put_cell(&self, key: &[u8], cell_id: CellId, value: &[u8]) -> Result<(), EncryptError> {
        let ct = encrypt_cell(self.key_provider.as_ref(), cell_id, value)?;
        self.inner.put(key, &ct).map_err(EncryptError::from)
    }

    /// Retrieve and decrypt the value stored under `key`, using an explicit
    /// [`CellId`] for AAD verification.
    pub fn get_cell(&self, key: &[u8], cell_id: CellId) -> Result<Option<Vec<u8>>, EncryptError> {
        match self.inner.get(key).map_err(EncryptError::from)? {
            None => Ok(None),
            Some(ct) => {
                let pt = decrypt_cell(self.key_provider.as_ref(), cell_id, &ct)?;
                Ok(Some(pt))
            }
        }
    }

    /// Return a reference to the inner store (bypasses encryption).
    ///
    /// Use with caution — the inner store holds ciphertext.
    pub fn inner_ref(&self) -> &T {
        &self.inner
    }
}

// ── KvStore implementation ────────────────────────────────────────────────────

impl<T: KvStore, K: KeyProvider, A: crate::aead::Aead> KvStore for EncryptedKv<T, K, A> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let aad = derive_cell_id(key);
        match self.inner.get(key)? {
            None => Ok(None),
            Some(ct) => {
                let pt = self.decrypt_value(&aad, &ct).map_err(StoreError::from)?;
                Ok(Some(pt))
            }
        }
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let aad = derive_cell_id(key);
        let ct = self.encrypt_value(&aad, value).map_err(StoreError::from)?;
        self.inner.put(key, &ct)
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.inner.delete(key)
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        self.inner.contains(key)
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        // We need to clone the key and aead reference for use in the closure.
        // Clone the key bytes since we cannot send a reference.
        let key_bytes = *self.key_provider.key32().map_err(StoreError::from)?;
        // SAFETY: the references into self (inner, aead) are valid for 'a.
        // We must capture them by cloning what we need.
        // Since A: Aead + Clone isn't guaranteed, we use the AeadKind enum.
        // Instead, we capture key+aead as a pair via a closure over refs.

        // Build a Vec of decrypted items upfront (avoids lifetime issues with
        // capturing &self through a closure that outlives the borrow).
        let raw_items: Vec<_> = self.inner.range(lo, hi)?.collect::<Vec<_>>();
        let mut decrypted = Vec::with_capacity(raw_items.len());
        for item in raw_items {
            let (k, ct) = item?;
            let aad = derive_cell_id(&k);
            let pt =
                decrypt_with_aead(&self.aead, &key_bytes, &aad, &ct).map_err(StoreError::from)?;
            decrypted.push(Ok((k, pt)));
        }
        Ok(Box::new(decrypted.into_iter()))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        let inner_txn = self.inner.transaction()?;
        let key = *self.key_provider.key32().map_err(StoreError::from)?;

        // We need A: Clone for EncryptedTxn. Use the AeadKind enum to
        // carry the aead choice, since AeadKind impls Aead + Clone + Copy.
        // Detect which variant is in use by checking nonce_len.
        let kind = if self.aead.nonce_len() == 12 {
            AeadKind::AesGcmSiv256
        } else {
            AeadKind::XChaCha20Poly1305
        };

        Ok(Box::new(EncryptedTxn::new(inner_txn, key, kind)))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        let inner_snap = self.inner.snapshot()?;
        let key = *self.key_provider.key32().map_err(StoreError::from)?;

        let kind = if self.aead.nonce_len() == 12 {
            AeadKind::AesGcmSiv256
        } else {
            AeadKind::XChaCha20Poly1305
        };

        Ok(Box::new(EncryptedSnapshot::new(inner_snap, key, kind)))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let key_bytes = *self.key_provider.key32().map_err(StoreError::from)?;

        let raw_items: Vec<_> = self.inner.iter()?.collect::<Vec<_>>();
        let mut decrypted = Vec::with_capacity(raw_items.len());
        for item in raw_items {
            let (k, ct) = item?;
            let aad = derive_cell_id(&k);
            let pt =
                decrypt_with_aead(&self.aead, &key_bytes, &aad, &ct).map_err(StoreError::from)?;
            decrypted.push(Ok((k, pt)));
        }
        Ok(Box::new(decrypted.into_iter()))
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.inner.flush()
    }
}

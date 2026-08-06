//! Envelope-encrypted transaction wrapper for `oxistore-encrypt`.
//!
//! [`EnvelopeTxn`] wraps any [`KvTxn`] and transparently applies the
//! [`EnvelopeCipher`] to written values and read values.
//!
//! # AAD consistency
//!
//! The envelope layer ([`EncryptedKvEnvelope`](crate::EncryptedKvEnvelope))
//! binds each ciphertext to its storage location by using the **raw KV key
//! bytes** as AEAD associated data (AAD).  This transaction wrapper mirrors that
//! exactly: `get`/`put`/`range` all pass the raw key as AAD.  It therefore does
//! **not** reuse the cell-layer [`EncryptedTxn`](crate::EncryptedTxn), which
//! uses a BLAKE3-derived cell id as AAD and is wire-incompatible with the
//! envelope format.

use oxistore_core::{KvTxn, RangeIter, StoreError};

use crate::envelope::EnvelopeCipher;

/// An envelope-encrypted write transaction.
///
/// Obtained via [`EncryptedKvEnvelope::transaction`](crate::EncryptedKvEnvelope)
/// (through the [`KvStore`](oxistore_core::KvStore) trait).  Values are
/// encrypted on [`put`](KvTxn::put) and decrypted on [`get`](KvTxn::get) using
/// the same envelope wire format as the store.  Commit/rollback delegate to the
/// inner transaction, so atomicity is provided entirely by the backend.
pub struct EnvelopeTxn<'a> {
    inner: Box<dyn KvTxn + 'a>,
    cipher: EnvelopeCipher,
}

impl<'a> EnvelopeTxn<'a> {
    /// Wrap `inner` with the given envelope `cipher`.
    pub(crate) fn new(inner: Box<dyn KvTxn + 'a>, cipher: EnvelopeCipher) -> Self {
        Self { inner, cipher }
    }
}

impl KvTxn for EnvelopeTxn<'_> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        match self.inner.get(key)? {
            None => Ok(None),
            Some(ct) => {
                let pt = self.cipher.decrypt(&ct, key).map_err(StoreError::from)?;
                Ok(Some(pt))
            }
        }
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let ct = self.cipher.encrypt(value, key).map_err(StoreError::from)?;
        self.inner.put(key, &ct)
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StoreError> {
        self.inner.delete(key)
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        self.inner.contains(key)
    }

    fn range<'s>(&'s self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'s>, StoreError> {
        // Materialise the inner range, decrypting each value with its own raw
        // key as AAD (mirroring `EncryptedKvEnvelope::range`).
        let raw_items: Vec<_> = self.inner.range(lo, hi)?.collect();
        let mut decrypted = Vec::with_capacity(raw_items.len());
        for item in raw_items {
            let (k, ct) = item?;
            let pt = self.cipher.decrypt(&ct, &k).map_err(StoreError::from)?;
            decrypted.push(Ok((k, pt)));
        }
        Ok(Box::new(decrypted.into_iter()))
    }

    fn commit(self: Box<Self>) -> Result<(), StoreError> {
        self.inner.commit()
    }

    fn rollback(self: Box<Self>) -> Result<(), StoreError> {
        self.inner.rollback()
    }
}

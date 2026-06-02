//! Encrypted transaction wrapper for `oxistore-encrypt`.
//!
//! [`EncryptedTxn`] wraps any [`KvTxn`] and transparently encrypts written
//! values and decrypts read values, using BLAKE3-derived cell IDs as AAD.

use oxistore_core::{KvTxn, RangeIter, StoreError};

use crate::aead::{decrypt_with_aead, derive_cell_id, encrypt_with_aead, AeadKind};

/// An encrypted write transaction.
///
/// Obtained via `EncryptedKv::transaction` (through the `KvStore` trait).
/// Values are encrypted on [`put`](KvTxn::put) and decrypted on
/// [`get`](KvTxn::get).  Commit/rollback delegate to the inner transaction.
pub struct EncryptedTxn<'a> {
    inner: Box<dyn KvTxn + 'a>,
    key: [u8; 32],
    aead: AeadKind,
}

impl<'a> EncryptedTxn<'a> {
    /// Wrap `inner` with the given `key` and `aead` choice.
    pub(crate) fn new(inner: Box<dyn KvTxn + 'a>, key: [u8; 32], aead: AeadKind) -> Self {
        Self { inner, key, aead }
    }
}

impl<'a> KvTxn for EncryptedTxn<'a> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let aad = derive_cell_id(key);
        match self.inner.get(key)? {
            None => Ok(None),
            Some(ct) => {
                let pt = decrypt_with_aead(&self.aead, &self.key, &aad, &ct)
                    .map_err(StoreError::from)?;
                Ok(Some(pt))
            }
        }
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let aad = derive_cell_id(key);
        let ct = encrypt_with_aead(&self.aead, &self.key, &aad, value).map_err(StoreError::from)?;
        self.inner.put(key, &ct)
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StoreError> {
        self.inner.delete(key)
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        self.inner.contains(key)
    }

    fn range<'s>(&'s self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'s>, StoreError> {
        let key_bytes = self.key;
        let aead = self.aead;
        let raw_items: Vec<_> = self.inner.range(lo, hi)?.collect();
        let mut decrypted = Vec::with_capacity(raw_items.len());
        for item in raw_items {
            let (k, ct) = item?;
            let aad = derive_cell_id(&k);
            let pt = decrypt_with_aead(&aead, &key_bytes, &aad, &ct).map_err(StoreError::from)?;
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

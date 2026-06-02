//! Encrypted snapshot wrapper for `oxistore-encrypt`.
//!
//! [`EncryptedSnapshot`] wraps any [`KvSnapshot`] and transparently decrypts
//! values on read, using BLAKE3-derived cell IDs as AAD.

use oxistore_core::{KvSnapshot, RangeIter, StoreError};

use crate::aead::{decrypt_with_aead, derive_cell_id, AeadKind};

/// A point-in-time read-only encrypted snapshot.
///
/// Obtained via `EncryptedKv::snapshot` (through the `KvStore` trait).
/// All reads decrypt values transparently.  The snapshot reflects the state
/// of the store at the moment it was captured; subsequent writes are not
/// visible.
pub struct EncryptedSnapshot<'a> {
    inner: Box<dyn KvSnapshot + 'a>,
    key: [u8; 32],
    aead: AeadKind,
}

impl<'a> EncryptedSnapshot<'a> {
    /// Wrap `inner` with the given `key` and `aead` choice.
    pub(crate) fn new(inner: Box<dyn KvSnapshot + 'a>, key: [u8; 32], aead: AeadKind) -> Self {
        Self { inner, key, aead }
    }
}

impl<'a> KvSnapshot for EncryptedSnapshot<'a> {
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
}

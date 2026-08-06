//! Envelope-encrypted snapshot wrapper for `oxistore-encrypt`.
//!
//! [`EnvelopeSnapshot`] wraps any [`KvSnapshot`] and transparently decrypts
//! values on read using the [`EnvelopeCipher`].
//!
//! Like [`EnvelopeTxn`](crate::EnvelopeTxn), it uses the **raw KV key bytes** as
//! AEAD associated data, matching the envelope wire format written by
//! [`EncryptedKvEnvelope`](crate::EncryptedKvEnvelope).  It therefore does not
//! reuse the cell-layer [`EncryptedSnapshot`](crate::EncryptedSnapshot).

use oxistore_core::{KvSnapshot, RangeIter, StoreError};

use crate::envelope::EnvelopeCipher;

/// A point-in-time, read-only envelope-encrypted snapshot.
///
/// Obtained via [`EncryptedKvEnvelope::snapshot`](crate::EncryptedKvEnvelope)
/// (through the [`KvStore`](oxistore_core::KvStore) trait).  All reads decrypt
/// values transparently.  Point-in-time isolation is provided entirely by the
/// underlying snapshot; this wrapper only adds decryption.
pub struct EnvelopeSnapshot<'a> {
    inner: Box<dyn KvSnapshot + 'a>,
    cipher: EnvelopeCipher,
}

impl<'a> EnvelopeSnapshot<'a> {
    /// Wrap `inner` with the given envelope `cipher`.
    pub(crate) fn new(inner: Box<dyn KvSnapshot + 'a>, cipher: EnvelopeCipher) -> Self {
        Self { inner, cipher }
    }
}

impl KvSnapshot for EnvelopeSnapshot<'_> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        match self.inner.get(key)? {
            None => Ok(None),
            Some(ct) => {
                let pt = self.cipher.decrypt(&ct, key).map_err(StoreError::from)?;
                Ok(Some(pt))
            }
        }
    }

    fn range<'s>(&'s self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'s>, StoreError> {
        let raw_items: Vec<_> = self.inner.range(lo, hi)?.collect();
        let mut decrypted = Vec::with_capacity(raw_items.len());
        for item in raw_items {
            let (k, ct) = item?;
            let pt = self.cipher.decrypt(&ct, &k).map_err(StoreError::from)?;
            decrypted.push(Ok((k, pt)));
        }
        Ok(Box::new(decrypted.into_iter()))
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        self.inner.contains(key)
    }
}

#![cfg(feature = "oxicrypto-aws-lc")]
//! Integration test: aws-lc-rs backed AEAD as an alternative cipher for
//! `oxistore-encrypt` cell-level encryption.
//!
//! ## What this verifies
//!
//! - [`oxistore_encrypt::AwsLcOxistoreAead`] — the library bridge newtype wrapping
//!   `AwsLcAead` (oxicrypto-adapter-aws-lc) that implements the
//!   `oxistore_encrypt::aead::Aead` trait.
//! - `EncryptedKv::with_aead(inner, key, AwsLcOxistoreAead::aes256_gcm_siv())` — a
//!   transparent AES-256-GCM-SIV encrypted KV store backed by aws-lc-rs.
//! - End-to-end `put` + `get` round-trip produces the original plaintext.
//! - Raw ciphertext stored in the inner store is meaningfully different from plaintext.
//! - Authentication failure is detected: corrupting a stored ciphertext causes `get`
//!   to return an error.
//!
//! ## Design
//!
//! `oxistore_encrypt::Aead` and `oxicrypto_core::Aead` are two distinct traits in
//! different crates with incompatible signatures (the store trait uses `&[u8; 32]`
//! fixed-size key references and returns `Vec<u8>`; the crypto trait uses `&[u8]`
//! slices and writes into a pre-allocated output buffer).  The `AwsLcOxistoreAead`
//! bridge (now library code in `oxistore-encrypt`) reconciles the two.

use std::collections::HashMap;
use std::sync::Mutex;

use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
use oxistore_encrypt::{AwsLcOxistoreAead, EncryptedKv, StaticKey};

// ── Minimal in-memory KvStore for tests ───────────────────────────────────

#[derive(Default, Debug)]
struct MemStore(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

impl KvStore for MemStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?
            .get(key)
            .cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?
            .remove(key);
        Ok(())
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?
            .contains_key(key))
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let guard = self
            .0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?;
        let lo = lo.to_vec();
        let hi = hi.to_vec();
        let pairs: Vec<_> = guard
            .iter()
            .filter(|(k, _)| **k >= lo && **k < hi)
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        drop(guard);
        Ok(Box::new(pairs.into_iter()))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Err(StoreError::Other("MemStore: no transactions".into()))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Other("MemStore: no snapshots".into()))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let guard = self
            .0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?;
        let pairs: Vec<_> = guard
            .iter()
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        drop(guard);
        Ok(Box::new(pairs.into_iter()))
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// Put + get round-trip with AES-256-GCM-SIV as the cell-level cipher.
#[test]
fn awslc_aes256gcmsiv_cell_round_trip() {
    let inner = MemStore::default();
    let key_bytes = [0x42u8; 32];
    let key = StaticKey::from_array(key_bytes);
    let enc = EncryptedKv::with_aead(inner, key, AwsLcOxistoreAead::aes256_gcm_siv());

    enc.put(b"hello", b"world").expect("put");
    let got = enc.get(b"hello").expect("get").expect("present");
    assert_eq!(got, b"world");
}

/// The raw ciphertext stored in the inner store must differ from the plaintext.
#[test]
fn awslc_aes256gcm_ciphertext_differs_from_plaintext() {
    let inner = MemStore::default();
    let key_bytes = [0x11u8; 32];
    let key = StaticKey::from_array(key_bytes);
    let enc = EncryptedKv::with_aead(inner, key, AwsLcOxistoreAead::aes256_gcm());

    let pt = b"aws-lc cell encryption test value";
    enc.put(b"k1", pt).expect("put");

    // Bypass decryption by reading from the inner store directly.
    let raw_ct = enc
        .inner_ref()
        .get(b"k1")
        .expect("inner get")
        .expect("present in inner store");

    // The ciphertext must not equal the plaintext.
    assert_ne!(
        raw_ct,
        pt.as_slice(),
        "ciphertext must differ from plaintext"
    );

    // The ciphertext must be longer (nonce prepended + tag appended).
    assert!(
        raw_ct.len() > pt.len(),
        "ciphertext ({}) must be longer than plaintext ({})",
        raw_ct.len(),
        pt.len()
    );
}

/// Multiple distinct keys produce distinct ciphertexts (cell-binding via AAD).
#[test]
fn awslc_chacha20_multiple_keys_distinct_ciphertexts() {
    let inner = MemStore::default();
    let key_bytes = [0xdcu8; 32];
    let key = StaticKey::from_array(key_bytes);
    let enc = EncryptedKv::with_aead(inner, key, AwsLcOxistoreAead::chacha20_poly1305());

    let pt = b"same plaintext for both cells";
    enc.put(b"cell:1", pt).expect("put cell:1");
    enc.put(b"cell:2", pt).expect("put cell:2");

    let ct1 = enc
        .inner_ref()
        .get(b"cell:1")
        .expect("inner get 1")
        .expect("present");
    let ct2 = enc
        .inner_ref()
        .get(b"cell:2")
        .expect("inner get 2")
        .expect("present");

    // Ciphertexts are bound to their storage key (different AAD), so they differ.
    assert_ne!(
        ct1, ct2,
        "same plaintext under different keys must produce distinct ciphertexts"
    );

    // Decryption must still recover the original plaintext.
    let recovered1 = enc.get(b"cell:1").expect("get 1").expect("present");
    let recovered2 = enc.get(b"cell:2").expect("get 2").expect("present");
    assert_eq!(recovered1, pt.as_slice());
    assert_eq!(recovered2, pt.as_slice());
}

/// Corrupting one byte of the raw ciphertext must cause authentication failure.
#[test]
fn awslc_aes256gcmsiv_authentication_failure_on_corruption() {
    let inner = MemStore::default();
    let key_bytes = [0xefu8; 32];
    let key = StaticKey::from_array(key_bytes);
    let enc = EncryptedKv::with_aead(inner, key, AwsLcOxistoreAead::aes256_gcm_siv());

    enc.put(b"secret", b"very secret value").expect("put");

    // Corrupt the last byte of the ciphertext (auth tag area).
    let raw_ct = enc
        .inner_ref()
        .get(b"secret")
        .expect("inner get")
        .expect("present");
    let mut corrupted = raw_ct.clone();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0xff;
    enc.inner_ref()
        .put(b"secret", &corrupted)
        .expect("inner put corrupted");

    // Decryption must fail because the tag no longer matches.
    let result = enc.get(b"secret");
    assert!(
        result.is_err(),
        "decryption of a corrupted ciphertext must fail, got: {result:?}"
    );
}

/// Absent key returns `None` (no panic, no spurious decryption).
#[test]
fn awslc_get_absent_key_returns_none() {
    let inner = MemStore::default();
    let key_bytes = [0x00u8; 32];
    let key = StaticKey::from_array(key_bytes);
    let enc = EncryptedKv::with_aead(inner, key, AwsLcOxistoreAead::aes256_gcm_siv());

    let result = enc.get(b"not_present").expect("get absent key");
    assert!(result.is_none(), "absent key must return None");
}

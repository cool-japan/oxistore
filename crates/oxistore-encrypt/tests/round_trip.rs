//! Round-trip tests for `oxistore-encrypt`.
//!
//! These tests verify that:
//! - Values written with `EncryptedKv::put` are correctly recovered via `get`.
//! - Identical plaintexts produce distinct ciphertexts (nonce uniqueness).
//! - The raw ciphertext is meaningfully different from the plaintext.

use std::collections::HashMap;
use std::sync::Mutex;

use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
use oxistore_encrypt::{decrypt_cell, encrypt_cell, CellId, EncryptedKv, StaticKey};

// ── Minimal in-memory KvStore for tests ──────────────────────────────────────

#[derive(Default, Debug)]
struct MemStore(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

impl KvStore for MemStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.0.lock().expect("lock poisoned").get(key).cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.0
            .lock()
            .expect("lock poisoned")
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.0.lock().expect("lock poisoned").remove(key);
        Ok(())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let guard = self.0.lock().expect("lock poisoned");
        let lo = lo.to_vec();
        let hi = hi.to_vec();
        let pairs: Vec<_> = guard
            .iter()
            .filter(|(k, _)| **k >= lo && **k < hi)
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        // Must drop guard before returning to avoid holding across iterator lifetime.
        drop(guard);
        Ok(Box::new(pairs.into_iter()))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Err(StoreError::Other(
            "MemStore: no transaction support".to_string(),
        ))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Other(
            "MemStore: no snapshot support".to_string(),
        ))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let guard = self.0.lock().expect("lock poisoned");
        let mut pairs: Vec<(Vec<u8>, Vec<u8>)> =
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        drop(guard);
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        Ok(Box::new(pairs.into_iter().map(Ok)))
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn test_key() -> StaticKey {
    StaticKey::from_array([0x42u8; 32])
}

fn make_enc() -> EncryptedKv<MemStore, StaticKey> {
    EncryptedKv::new(MemStore::default(), test_key())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn basic_put_get_round_trip() {
    let enc = make_enc();
    enc.put(b"hello", b"world").expect("put failed");
    let result = enc.get(b"hello").expect("get failed");
    assert_eq!(result, Some(b"world".to_vec()));
}

#[test]
fn absent_key_returns_none() {
    let enc = make_enc();
    let result = enc.get(b"nonexistent").expect("get failed");
    assert_eq!(result, None);
}

#[test]
fn overwrite_returns_latest_value() {
    let enc = make_enc();
    enc.put(b"k", b"v1").expect("first put failed");
    enc.put(b"k", b"v2").expect("second put failed");
    let result = enc.get(b"k").expect("get failed");
    assert_eq!(result, Some(b"v2".to_vec()));
}

#[test]
fn multiple_keys_round_trip() {
    let enc = make_enc();
    let pairs: &[(&[u8], &[u8])] = &[(b"alpha", b"AAA"), (b"beta", b"BBB"), (b"gamma", b"CCC")];
    for (k, v) in pairs {
        enc.put(k, v).expect("put failed");
    }
    for (k, v) in pairs {
        let result = enc.get(k).expect("get failed");
        assert_eq!(result.as_deref(), Some(*v), "mismatch for key {k:?}");
    }
}

#[test]
fn delete_removes_entry() {
    let enc = make_enc();
    enc.put(b"gone", b"value").expect("put failed");
    enc.delete(b"gone").expect("delete failed");
    let result = enc.get(b"gone").expect("get after delete failed");
    assert_eq!(result, None);
}

#[test]
fn empty_plaintext_round_trips() {
    let enc = make_enc();
    enc.put(b"empty", b"").expect("put failed");
    let result = enc.get(b"empty").expect("get failed");
    assert_eq!(result, Some(vec![]));
}

#[test]
fn nonce_uniqueness_identical_plaintexts() {
    // Two puts of the same plaintext must produce distinct ciphertexts.
    // The first 24 bytes of each stored value are the random nonce.
    let inner = MemStore::default();
    let enc = EncryptedKv::new(inner, test_key());

    enc.put(b"key1", b"hello world").expect("put key1 failed");
    enc.put(b"key2", b"hello world").expect("put key2 failed");

    // Access the raw (encrypted) bytes by going through a second MemStore
    // reference — not possible here because MemStore is owned; instead we
    // call encrypt_cell twice and check the nonces differ.
    let key = test_key();
    let cell_id = CellId {
        table_id: 0,
        row_id: 0,
        col_id: 0,
    };
    let ct1 = encrypt_cell(&key, cell_id, b"hello world").expect("encrypt 1 failed");
    let ct2 = encrypt_cell(&key, cell_id, b"hello world").expect("encrypt 2 failed");

    // Nonces are the first 24 bytes.
    assert_ne!(
        &ct1[..24],
        &ct2[..24],
        "identical plaintexts must produce distinct nonces"
    );
    // Both must decrypt correctly.
    let pt1 = decrypt_cell(&key, cell_id, &ct1).expect("decrypt 1 failed");
    let pt2 = decrypt_cell(&key, cell_id, &ct2).expect("decrypt 2 failed");
    assert_eq!(pt1, b"hello world");
    assert_eq!(pt2, b"hello world");
}

#[test]
fn ciphertext_differs_from_plaintext() {
    let key = test_key();
    let cell_id = CellId {
        table_id: 1,
        row_id: 2,
        col_id: 3,
    };
    let plaintext = b"sensitive data";
    let ct = encrypt_cell(&key, cell_id, plaintext).expect("encrypt failed");

    // The output is longer (nonce + ciphertext + tag > plaintext).
    assert!(ct.len() > plaintext.len());

    // The raw plaintext bytes do not appear verbatim in the output.
    let ct_window = &ct[24..ct.len() - 16]; // ciphertext body (without nonce or tag)
    assert_ne!(
        ct_window, plaintext,
        "ciphertext body must not equal plaintext"
    );
}

#[test]
fn wrong_cell_id_aad_rejected() {
    // Encrypt for cell (1,2,3), attempt to decrypt as cell (9,9,9) → error.
    let key = test_key();
    let cell_a = CellId {
        table_id: 1,
        row_id: 2,
        col_id: 3,
    };
    let cell_b = CellId {
        table_id: 9,
        row_id: 9,
        col_id: 9,
    };

    let ct = encrypt_cell(&key, cell_a, b"location-bound data").expect("encrypt failed");
    let result = decrypt_cell(&key, cell_b, &ct);
    assert!(result.is_err(), "wrong CellId must cause auth failure");
}

#[test]
fn keyring_key_returns_error() {
    use oxistore_encrypt::KeyringKey;
    let provider = KeyringKey::new("test-label");
    let cell_id = CellId {
        table_id: 0,
        row_id: 0,
        col_id: 0,
    };
    let result = encrypt_cell(&provider, cell_id, b"data");
    assert!(result.is_err(), "KeyringKey stub must return an error");
}

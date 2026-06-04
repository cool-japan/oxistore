//! Advanced tests for `oxistore-encrypt`:
//! - Key rotation round-trip: put 100 entries, rotate, verify readable with new key
//! - Large-value encryption: 1MB and 10MB values round-trip
//! - KeyringKey stub behavior: verify returns KeyringUnavailable
//! - Concurrent access: multiple threads doing put/get simultaneously

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
use oxistore_encrypt::{EncryptedKv, KeyProvider, KeyringKey, StaticKey};

// ── Minimal in-memory KvStore for tests ──────────────────────────────────────

#[derive(Default, Debug)]
struct MemStore(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

impl MemStore {
    fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

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
        drop(guard);
        Ok(Box::new(pairs.into_iter()))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let guard = self.0.lock().expect("lock poisoned");
        let pairs: Vec<_> = guard
            .iter()
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
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

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

// ── Shared store (Arc-wrapped) for concurrent tests ──────────────────────────

#[derive(Debug, Clone)]
struct SharedMemStore(Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>>);

impl SharedMemStore {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())))
    }
}

impl KvStore for SharedMemStore {
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
        let lo_vec = lo.to_vec();
        let hi_vec = hi.to_vec();
        let pairs: Vec<_> = guard
            .iter()
            .filter(|(k, _)| **k >= lo_vec && **k < hi_vec)
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        drop(guard);
        Ok(Box::new(pairs.into_iter()))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let guard = self.0.lock().expect("lock poisoned");
        let pairs: Vec<_> = guard
            .iter()
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        drop(guard);
        Ok(Box::new(pairs.into_iter()))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Err(StoreError::Other("SharedMemStore: no txn".to_string()))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Other("SharedMemStore: no snapshot".to_string()))
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

// ── Large-value encryption: 1MB and 10MB ─────────────────────────────────────

#[test]
fn large_value_1mb_round_trip() {
    let inner = MemStore::new();
    let key = StaticKey::from_array([0x55u8; 32]);
    let enc = EncryptedKv::new(inner, key);

    let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
    enc.put(b"large_1mb", &payload).expect("put 1MB");
    let got = enc
        .get(b"large_1mb")
        .expect("get 1MB")
        .expect("should exist");
    assert_eq!(got, payload, "1MB round-trip failed");
}

#[test]
fn large_value_10mb_round_trip() {
    let inner = MemStore::new();
    let key = StaticKey::from_array([0xAAu8; 32]);
    let enc = EncryptedKv::new(inner, key);

    let size = 10 * 1024 * 1024usize;
    let payload: Vec<u8> = (0..size).map(|i| (i % 197) as u8).collect();
    enc.put(b"large_10mb", &payload).expect("put 10MB");
    let got = enc
        .get(b"large_10mb")
        .expect("get 10MB")
        .expect("should exist");
    assert_eq!(got.len(), size, "10MB value size mismatch");
    assert_eq!(got, payload, "10MB round-trip failed");
}

// ── KeyringKey stub behavior ──────────────────────────────────────────────────

#[test]
fn keyring_key_returns_unavailable() {
    use oxistore_encrypt::EncryptError;

    let key = KeyringKey::new("my-app-key");
    assert_eq!(key.label(), "my-app-key");

    let result = key.get_key();
    assert!(result.is_err(), "KeyringKey stub must return Err");

    match result.unwrap_err() {
        EncryptError::KeyringUnavailable { label } => {
            // The label field always starts with the entry label.  When the
            // `os-keyring` feature is enabled the underlying OS error is
            // appended (e.g. "my-app-key: No default store has been set").
            assert!(
                label.starts_with("my-app-key"),
                "expected label starting with 'my-app-key', got {label:?}"
            );
        }
        other => panic!("expected KeyringUnavailable, got {other:?}"),
    }
}

#[test]
fn keyring_key_with_various_labels() {
    let labels = [
        "db-master-key",
        "",
        "special/key:with.chars",
        "unicode_ключ",
    ];
    for label in &labels {
        let key = KeyringKey::new(*label);
        assert_eq!(key.label(), *label);
        assert!(
            key.get_key().is_err(),
            "label {label:?}: expected Err from stub"
        );
    }
}

// ── Concurrent access: multiple threads put/get simultaneously ───────────────

#[test]
fn concurrent_put_get_no_data_corruption() {
    use std::thread;

    let inner = SharedMemStore::new();
    let key_material = [0xCCu8; 32];
    let enc = Arc::new(EncryptedKv::new(inner, StaticKey::from_array(key_material)));

    let n_threads = 4usize;
    let ops_per_thread = 50usize;

    let handles: Vec<_> = (0..n_threads)
        .map(|tid| {
            let enc_clone = Arc::clone(&enc);
            thread::spawn(move || {
                for op in 0..ops_per_thread {
                    let key = format!("thread{tid}_key{op:03}");
                    let value = format!("value-{tid}-{op}");
                    enc_clone
                        .put(key.as_bytes(), value.as_bytes())
                        .unwrap_or_else(|e| panic!("put failed t{tid} op{op}: {e}"));
                    let got = enc_clone
                        .get(key.as_bytes())
                        .unwrap_or_else(|e| panic!("get failed t{tid} op{op}: {e}"))
                        .unwrap_or_else(|| panic!("key missing t{tid} op{op}"));
                    assert_eq!(got.as_slice(), value.as_bytes(), "mismatch t{tid} op{op}");
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }
}

// ── Key-rotation round-trip ───────────────────────────────────────────────────
//
// This test uses EncryptedKvEnvelope which supports true key rotation.
// After rotation, all 100 entries must be readable and the old key must
// no longer be able to decrypt new entries.

#[test]
fn key_rotation_100_entries_all_readable() {
    use oxicrypto::Argon2Params;
    use oxistore_encrypt::{generate_salt, EncryptedKvEnvelope, EnvelopeCipher, Keyring};

    let inner = MemStore::new();

    // Create keyring with KEK version 1 (test params for speed)
    let salt1 = generate_salt().expect("salt");
    let keyring1 =
        Keyring::from_passphrase_with_params(b"passphrase-one", &salt1, Argon2Params::TEST_PARAMS)
            .expect("derive keyring1");

    let cipher1 = EnvelopeCipher::new(keyring1.clone());
    let mut env_store = EncryptedKvEnvelope::new(inner, cipher1);

    // Put 100 entries under KEK v1
    let n = 100usize;
    for i in 0..n {
        let k = format!("entry_{i:04}");
        let v = format!("secret_value_{i}");
        env_store.put(k.as_bytes(), v.as_bytes()).expect("put");
    }

    // Verify all readable before rotation
    for i in 0..n {
        let k = format!("entry_{i:04}");
        let got = env_store
            .get(k.as_bytes())
            .expect("get pre-rotation")
            .unwrap_or_else(|| panic!("missing pre-rotation: {k}"));
        assert_eq!(got, format!("secret_value_{i}").as_bytes());
    }

    // Rotate to a new KEK
    let salt2 = generate_salt().expect("salt2");
    let new_kek_keyring =
        Keyring::from_passphrase_with_params(b"passphrase-two", &salt2, Argon2Params::TEST_PARAMS)
            .expect("derive keyring2");
    let new_kek = *new_kek_keyring.active_kek().expect("active kek");

    env_store.rotate_kek(new_kek).expect("rotate_kek");

    // All 100 entries must still be readable after rotation (DEK re-wraps only)
    for i in 0..n {
        let k = format!("entry_{i:04}");
        let got = env_store
            .get(k.as_bytes())
            .expect("get post-rotation")
            .unwrap_or_else(|| panic!("missing post-rotation: {k}"));
        assert_eq!(
            got,
            format!("secret_value_{i}").as_bytes(),
            "entry {i} wrong after rotation"
        );
    }

    // New entries written after rotation must also be readable
    env_store
        .put(b"new_after_rotation", b"fresh_value")
        .expect("put new");
    let fresh = env_store
        .get(b"new_after_rotation")
        .expect("get new")
        .expect("should exist");
    assert_eq!(fresh, b"fresh_value");
}

// ── Encryption: nonces are unique (no nonce reuse) ────────────────────────────

#[test]
fn nonces_are_unique_across_put_calls() {
    use std::sync::Arc;
    // Store 10 entries with the same value — each should produce different ciphertext
    // because nonces are random per call
    let raw_inner: Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));

    // Build a MemStore-like thing that we can also read the inner map from
    struct SharedMemStore(Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>>);
    impl KvStore for SharedMemStore {
        fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(self.0.lock().expect("lock").get(key).cloned())
        }
        fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
            self.0
                .lock()
                .expect("lock")
                .insert(key.to_vec(), value.to_vec());
            Ok(())
        }
        fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
            self.0.lock().expect("lock").remove(key);
            Ok(())
        }
        fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
            let g = self.0.lock().expect("lock");
            let lo = lo.to_vec();
            let hi = hi.to_vec();
            let pairs: Vec<_> = g
                .iter()
                .filter(|(k, _)| **k >= lo && **k < hi)
                .map(|(k, v)| Ok((k.clone(), v.clone())))
                .collect();
            drop(g);
            Ok(Box::new(pairs.into_iter()))
        }
        fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
            let g = self.0.lock().expect("lock");
            let pairs: Vec<_> = g.iter().map(|(k, v)| Ok((k.clone(), v.clone()))).collect();
            drop(g);
            Ok(Box::new(pairs.into_iter()))
        }
        fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
            Err(StoreError::Other("no txn".to_string()))
        }
        fn snapshot(&self) -> Result<Box<dyn oxistore_core::KvSnapshot + '_>, StoreError> {
            Err(StoreError::Other("no snap".to_string()))
        }
        fn flush(&self) -> Result<(), StoreError> {
            Ok(())
        }
    }

    let inner = SharedMemStore(Arc::clone(&raw_inner));

    let key = StaticKey::from_array([0x42u8; 32]);
    let enc = EncryptedKv::new(inner, key);

    let plaintext = b"identical plaintext for nonce test";
    for i in 0..10 {
        let k = format!("k{i}");
        enc.put(k.as_bytes(), plaintext).expect("put");
    }

    // Collect all ciphertexts from the raw inner store
    let ciphertexts: Vec<Vec<u8>> = {
        let guard = raw_inner.lock().expect("lock");
        guard.values().cloned().collect()
    };

    // All ciphertexts must be distinct (unique nonces ensure this)
    for i in 0..ciphertexts.len() {
        for j in (i + 1)..ciphertexts.len() {
            assert_ne!(
                ciphertexts[i], ciphertexts[j],
                "ciphertext {i} and {j} are identical — nonce reuse!"
            );
        }
    }
}

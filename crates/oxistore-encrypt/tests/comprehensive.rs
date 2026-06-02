//! Comprehensive advanced tests for `oxistore-encrypt`.
//!
//! Coverage:
//! - Large value round-trips (1 MB and 10 MB)
//! - Empty value round-trip
//! - Empty key round-trip
//! - Concurrent access across 4 threads
//! - Wrong key produces decryption error (not panic, not plaintext)
//! - Overwrite returns latest value
//! - Keyring version management
//! - Key rotation via EncryptedKvEnvelope: all 20 entries remain readable
//! - Passphrase-derived Keyring is deterministic and fast (TEST_PARAMS)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use oxicrypto::Argon2Params;
use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
use oxistore_encrypt::{EncryptedKv, EncryptedKvEnvelope, EnvelopeCipher, Keyring, StaticKey};

// ── MemStore — basic non-clonable in-memory KvStore ──────────────────────────

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

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        Ok(self.0.lock().expect("lock poisoned").contains_key(key))
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

// ── SharedMemStore — clonable, Arc-backed KvStore for the wrong-key test ─────

/// A clonable, Arc-backed in-memory KV store.
///
/// Allows two `EncryptedKv` instances to share the same backing store, which
/// is required to test that decryption with the wrong key returns an error.
#[derive(Clone, Default, Debug)]
struct SharedMemStore(Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>>);

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

    fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
        Ok(self.0.lock().expect("lock poisoned").contains_key(key))
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

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Err(StoreError::Other(
            "SharedMemStore: no transaction support".to_string(),
        ))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Other(
            "SharedMemStore: no snapshot support".to_string(),
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

fn make_enc() -> EncryptedKv<MemStore, StaticKey> {
    EncryptedKv::new(MemStore::default(), StaticKey::from_array([0x42u8; 32]))
}

// ── Test 1: encrypt_large_value_1mb ──────────────────────────────────────────

#[test]
fn encrypt_large_value_1mb() {
    let store = make_enc();
    let data = vec![0xABu8; 1_048_576];
    store.put(b"large_key", &data).expect("put large");
    let retrieved = store
        .get(b"large_key")
        .expect("get large")
        .expect("should be Some");
    assert_eq!(retrieved.len(), 1_048_576);
    assert_eq!(retrieved, data);
}

// ── Test 2: encrypt_large_value_10mb ─────────────────────────────────────────

#[test]
#[ignore = "slow: 10 MB AEAD encrypt+decrypt typically takes >10 s; run with --include-ignored"]
fn encrypt_large_value_10mb() {
    let store = make_enc();
    let data = vec![0xCDu8; 10_485_760];
    store.put(b"large_key_10mb", &data).expect("put 10mb");
    let retrieved = store
        .get(b"large_key_10mb")
        .expect("get 10mb")
        .expect("should be Some");
    assert_eq!(retrieved.len(), 10_485_760);
    assert_eq!(retrieved, data);
}

// ── Test 3: encrypt_empty_value ───────────────────────────────────────────────

#[test]
fn encrypt_empty_value() {
    let store = make_enc();
    store.put(b"empty_val_key", b"").expect("put empty value");
    let retrieved = store
        .get(b"empty_val_key")
        .expect("get empty value")
        .expect("should be Some");
    assert!(
        retrieved.is_empty(),
        "empty value should round-trip as empty slice"
    );
}

// ── Test 4: encrypt_empty_key ─────────────────────────────────────────────────

#[test]
fn encrypt_empty_key() {
    let store = make_enc();
    let value = b"value_under_empty_key";
    store.put(b"", value).expect("put with empty key");
    let retrieved = store
        .get(b"")
        .expect("get with empty key")
        .expect("should be Some");
    assert_eq!(retrieved, value, "value under empty key must round-trip");
}

// ── Test 5: encrypt_concurrent_4threads ──────────────────────────────────────

#[test]
fn encrypt_concurrent_4threads() {
    // EncryptedKv<MemStore, StaticKey> is Send+Sync via Arc<T> + Arc<K>
    // and Mutex inside MemStore.
    let store = Arc::new(EncryptedKv::new(
        MemStore::default(),
        StaticKey::from_array([0x11u8; 32]),
    ));

    let thread_count = 4usize;
    let keys_per_thread = 25usize;

    let handles: Vec<_> = (0..thread_count)
        .map(|t| {
            let s = Arc::clone(&store);
            std::thread::spawn(move || {
                for i in 0..keys_per_thread {
                    let key = format!("thread_{t}_key_{i:03}");
                    let val = format!("thread_{t}_val_{i:03}");
                    s.put(key.as_bytes(), val.as_bytes())
                        .expect("concurrent put failed");
                    let got = s
                        .get(key.as_bytes())
                        .expect("concurrent get failed")
                        .expect("key should be present after put");
                    assert_eq!(
                        got,
                        val.as_bytes(),
                        "thread {t}: round-trip mismatch for key {key}"
                    );
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    // Verify all 100 entries are visible from main thread.
    for t in 0..thread_count {
        for i in 0..keys_per_thread {
            let key = format!("thread_{t}_key_{i:03}");
            let val = format!("thread_{t}_val_{i:03}");
            let got = store
                .get(key.as_bytes())
                .expect("main-thread get failed")
                .expect("key should still be present");
            assert_eq!(
                got,
                val.as_bytes(),
                "main thread: wrong value for key {key}"
            );
        }
    }
}

// ── Test 6: encrypt_wrong_key_fails ──────────────────────────────────────────

#[test]
fn encrypt_wrong_key_fails() {
    // Create a shared backing store so both EncryptedKv instances read the
    // same raw (ciphertext) bytes.
    let backing = SharedMemStore::default();

    let key_a = StaticKey::from_array([0xAAu8; 32]);
    let enc_a = EncryptedKv::new(backing.clone(), key_a);
    enc_a
        .put(b"secret_key", b"sensitive value")
        .expect("put with key A");

    // Attempt to decrypt with a different key.
    let key_b = StaticKey::from_array([0xBBu8; 32]);
    let enc_b = EncryptedKv::new(backing, key_b);
    let result = enc_b.get(b"secret_key");

    assert!(
        result.is_err(),
        "wrong key must produce Err, not Ok(Some(plaintext)); got: {result:?}"
    );
}

// ── Test 7: encrypt_overwrite_key ─────────────────────────────────────────────

#[test]
fn encrypt_overwrite_key() {
    let store = make_enc();
    store.put(b"k", b"v1").expect("first put");
    store.put(b"k", b"v2").expect("second put (overwrite)");
    let result = store
        .get(b"k")
        .expect("get after overwrite")
        .expect("key must be present");
    assert_eq!(result, b"v2", "overwrite must return the latest value");
}

// ── Test 8: keyring_version_management ────────────────────────────────────────

#[test]
fn keyring_version_management() {
    let kek1 = [1u8; 32];
    let mut ring = Keyring::new(kek1);

    assert_eq!(ring.active_version().expect("active_version"), 1);

    let kek2 = [2u8; 32];
    ring.rotate(kek2).expect("rotate to v2");
    assert_eq!(ring.active_version().expect("active_version after v2"), 2);

    let kek3 = [3u8; 32];
    ring.rotate(kek3).expect("rotate to v3");
    assert_eq!(ring.active_version().expect("active_version after v3"), 3);

    assert!(
        ring.kek_for_version(1).is_some(),
        "v1 KEK must still be accessible"
    );
    assert!(
        ring.kek_for_version(2).is_some(),
        "v2 KEK must still be accessible"
    );
    assert!(
        ring.kek_for_version(3).is_some(),
        "v3 KEK must still be accessible"
    );
    assert!(
        ring.kek_for_version(99).is_none(),
        "version 99 must not exist"
    );

    let versions = ring.version_numbers();
    assert_eq!(
        versions,
        vec![1, 2, 3],
        "version list must be sorted ascending"
    );
}

// ── Test 9: encrypt_key_rotation_all_entries_readable ─────────────────────────

#[test]
fn encrypt_key_rotation_all_entries_readable() {
    // EncryptedKvEnvelope supports key rotation via rotate_kek.
    let kek_v1 = [0x11u8; 32];
    let keyring = Keyring::new(kek_v1);
    let cipher = EnvelopeCipher::new(keyring);
    let mut env_wrapper = EncryptedKvEnvelope::new(MemStore::default(), cipher);

    // Write 20 entries under KEK v1.
    for i in 0u32..20 {
        let key = format!("rot_key_{i:03}");
        let val = format!("rot_val_{i}");
        env_wrapper
            .put(key.as_bytes(), val.as_bytes())
            .expect("put before rotation");
    }

    // Rotate to KEK v2 — only DEK wrappers change, data is untouched.
    let kek_v2 = [0x22u8; 32];
    let rotated = env_wrapper
        .rotate_kek(kek_v2)
        .expect("rotate_kek must succeed");
    assert_eq!(rotated, 20, "all 20 entries must be re-wrapped");

    // Verify all 20 entries are still readable after rotation.
    for i in 0u32..20 {
        let key = format!("rot_key_{i:03}");
        let expected_val = format!("rot_val_{i}");
        let got = env_wrapper
            .get(key.as_bytes())
            .expect("get after rotation")
            .unwrap_or_else(|| panic!("key {key} missing after rotation"));
        assert_eq!(
            got,
            expected_val.as_bytes(),
            "value for {key} must survive key rotation"
        );
    }
}

// ── Test 10: keyring_from_passphrase_with_test_params ─────────────────────────

#[test]
fn keyring_from_passphrase_with_test_params() {
    let salt = [0u8; 32];
    let ring = Keyring::from_passphrase_with_params(b"test_pass", &salt, Argon2Params::TEST_PARAMS)
        .expect("from_passphrase_with_params must succeed");

    assert_eq!(
        ring.active_version().expect("active_version"),
        1,
        "freshly created ring must have version 1"
    );

    // Same passphrase + same salt must derive the same KEK (deterministic).
    let ring2 =
        Keyring::from_passphrase_with_params(b"test_pass", &salt, Argon2Params::TEST_PARAMS)
            .expect("second from_passphrase_with_params must succeed");

    assert_eq!(
        ring.kek_for_version(1),
        ring2.kek_for_version(1),
        "same passphrase+salt must produce identical KEK"
    );

    // Different passphrase must produce a different KEK.
    let ring3 =
        Keyring::from_passphrase_with_params(b"different_pass", &salt, Argon2Params::TEST_PARAMS)
            .expect("third from_passphrase_with_params must succeed");

    assert_ne!(
        ring.kek_for_version(1),
        ring3.kek_for_version(1),
        "different passphrase must produce a different KEK"
    );
}

// ── Test 11: encrypt_key_rotation_100_entries_all_readable_after_rotate ────────

#[test]
fn encrypt_key_rotation_100_entries_all_readable_after_rotate() {
    // Use SharedMemStore so we can share the same backing data between two
    // EncryptedKvEnvelope instances (needed for the "old key must fail" check).
    let backing = SharedMemStore::default();

    let kek_v1 = [0x11u8; 32];
    let keyring_v1 = Keyring::new(kek_v1);
    let cipher_v1 = EnvelopeCipher::new(keyring_v1);
    let mut env_wrapper = EncryptedKvEnvelope::new(backing.clone(), cipher_v1);

    // Write 100 entries under KEK v1.
    for i in 0u32..100 {
        let key = format!("rot100_key_{i:04}");
        let val = format!("rot100_val_{i}");
        env_wrapper
            .put(key.as_bytes(), val.as_bytes())
            .expect("put before rotation");
    }

    // Rotate to KEK v2 — only DEK wrappers change, bulk data is untouched.
    let kek_v2 = [0x22u8; 32];
    let rotated = env_wrapper
        .rotate_kek(kek_v2)
        .expect("rotate_kek must succeed");
    assert_eq!(rotated, 100, "all 100 entries must be re-wrapped");

    // Verify all 100 entries remain readable after rotation.
    for i in 0u32..100 {
        let key = format!("rot100_key_{i:04}");
        let expected_val = format!("rot100_val_{i}");
        let got = env_wrapper
            .get(key.as_bytes())
            .expect("get after rotation must not error")
            .unwrap_or_else(|| panic!("key {key} is missing after rotation"));
        assert_eq!(
            got,
            expected_val.as_bytes(),
            "value for {key} must survive key rotation"
        );
    }

    // Attempt to read using ONLY the old KEK v1 → must fail because DEKs are
    // now wrapped under KEK v2.
    let old_keyring = Keyring::new(kek_v1);
    let old_cipher = EnvelopeCipher::new(old_keyring);
    let env_old = EncryptedKvEnvelope::new(backing, old_cipher);
    let first_key = b"rot100_key_0000";
    let result = env_old.get(first_key);
    assert!(
        result.is_err(),
        "reading with the revoked KEK v1 after rotation must return Err; got: {result:?}"
    );
}

// ── Test 12: encrypt_nonce_uniqueness_across_500_puts ─────────────────────────

#[test]
fn encrypt_nonce_uniqueness_across_500_puts() {
    // EncryptedKv encrypts each value with a randomly generated nonce.
    // With XChaCha20-Poly1305 the default AEAD, the nonce is 24 bytes
    // prepended to each stored ciphertext.
    //
    // Even if we didn't check nonces directly, distinct ciphertexts imply
    // distinct nonces: GCM-family / XChaCha20-Poly1305 with the same key+AAD
    // but different nonces always produces different ciphertexts, and two
    // identical nonces for the same plaintext would produce the same ciphertext.
    //
    // Strategy: collect the raw ciphertexts via inner_ref() and assert all 500
    // are distinct.  This is a probabilistic test that passes with overwhelming
    // probability when nonces are drawn from a CSPRNG.

    // Use SharedMemStore so we can call inner_ref().get() to read raw ciphertexts.
    let backing = SharedMemStore::default();
    let enc = EncryptedKv::new(backing.clone(), StaticKey::from_array([0x77u8; 32]));

    let n = 500usize;
    let plaintext = b"same_plaintext_every_time";

    for i in 0..n {
        let key = format!("nonce_test_{i:04}");
        enc.put(key.as_bytes(), plaintext)
            .unwrap_or_else(|e| panic!("put {i} failed: {e}"));
    }

    // Collect raw ciphertexts from the backing store.
    let mut ciphertexts = std::collections::HashSet::new();
    for i in 0..n {
        let key = format!("nonce_test_{i:04}");
        let raw = backing
            .get(key.as_bytes())
            .unwrap_or_else(|e| panic!("raw get {i} failed: {e}"))
            .unwrap_or_else(|| panic!("raw get {i} returned None"));
        ciphertexts.insert(raw);
    }

    assert_eq!(
        ciphertexts.len(),
        n,
        "all {n} ciphertexts must be distinct (nonce uniqueness across puts)"
    );
}

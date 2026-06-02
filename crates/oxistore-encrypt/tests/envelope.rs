//! Envelope encryption tests for `oxistore-encrypt`.
//!
//! Tests cover:
//! 1. Envelope round-trip (encrypt with keyring v1 → decrypt → verify)
//! 2. Key rotation (10 values encrypted under KEK v1; rotate to v2; all decrypt)
//! 3. Wrong passphrase fails
//! 4. Passphrase round-trip (derive, encrypt, re-derive same, decrypt)
//! 5. Version isolation (v1 ciphertext after rotate: requires v1 key to be present)
//! 6. EncryptedKvEnvelope integration (put/get/iter)
//! 7. Rotation count from rotate_all_keys

use std::collections::HashMap;
use std::sync::Mutex;

use oxicrypto::Argon2Params;
use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
use oxistore_encrypt::{
    generate_salt, rotate_all_keys, EncryptedKvEnvelope, EnvelopeCipher, Keyring,
};

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

// ── Test parameter helpers ────────────────────────────────────────────────────

/// Fast Argon2 params for tests (m=64 KiB, t=1, p=1).
const FAST_PARAMS: Argon2Params = Argon2Params::TEST_PARAMS;

fn make_keyring() -> Keyring {
    Keyring::new([0x42u8; 32])
}

fn make_cipher() -> EnvelopeCipher {
    EnvelopeCipher::new(make_keyring())
}

// ── Test 1: Envelope round-trip ───────────────────────────────────────────────

#[test]
fn envelope_round_trip() {
    let cipher = make_cipher();
    let plaintext = b"hello envelope world";
    let aad = b"cell-id-aad";

    let ct = cipher.encrypt(plaintext, aad).expect("encrypt failed");
    let recovered = cipher.decrypt(&ct, aad).expect("decrypt failed");

    assert_eq!(recovered, plaintext);
}

#[test]
fn envelope_round_trip_empty_plaintext() {
    let cipher = make_cipher();
    let ct = cipher.encrypt(b"", b"").expect("encrypt empty failed");
    let recovered = cipher.decrypt(&ct, b"").expect("decrypt empty failed");
    assert!(recovered.is_empty());
}

#[test]
fn envelope_round_trip_large_plaintext() {
    let cipher = make_cipher();
    let plaintext: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
    let ct = cipher
        .encrypt(&plaintext, b"large")
        .expect("encrypt large failed");
    let recovered = cipher.decrypt(&ct, b"large").expect("decrypt large failed");
    assert_eq!(recovered, plaintext);
}

#[test]
fn envelope_ciphertext_differs_from_plaintext() {
    let cipher = make_cipher();
    let plaintext = b"visible secret";
    let ct = cipher.encrypt(plaintext, b"").expect("encrypt failed");
    // The ciphertext must be strictly longer.
    assert!(ct.len() > plaintext.len());
    // The plaintext must not appear verbatim in the ciphertext.
    let found = ct.windows(plaintext.len()).any(|w| w == plaintext.as_ref());
    assert!(!found, "plaintext must not appear verbatim in ciphertext");
}

#[test]
fn envelope_nonce_uniqueness() {
    // Two encryptions of the same plaintext must produce distinct envelopes.
    let cipher = make_cipher();
    let pt = b"same plaintext";
    let ct1 = cipher.encrypt(pt, b"").expect("encrypt 1 failed");
    let ct2 = cipher.encrypt(pt, b"").expect("encrypt 2 failed");
    assert_ne!(ct1, ct2, "distinct nonces must produce distinct envelopes");
}

// ── Test 2: Key rotation ──────────────────────────────────────────────────────

#[test]
fn key_rotation_all_values_decrypt_after_rotate() {
    let cipher = make_cipher();

    // Encrypt 10 values under KEK v1.
    let count = 10usize;
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(count);
    for i in 0..count {
        let plaintext = format!("value-{i}").into_bytes();
        let aad = format!("key-{i}").into_bytes();
        let ct = cipher.encrypt(&plaintext, &aad).expect("encrypt failed");
        entries.push((plaintext, ct));
    }

    // Rotate to KEK v2.
    let new_kek = [0xBBu8; 32];
    cipher
        .add_kek_version(new_kek)
        .expect("add_kek_version failed");

    assert_eq!(cipher.active_version().expect("active_version failed"), 2);

    // All 10 ciphertexts (still under v1 wrapper) must decrypt via the cipher
    // that now holds both v1 and v2.
    for (i, (plaintext, ct)) in entries.iter().enumerate() {
        let aad = format!("key-{i}").into_bytes();
        let recovered = cipher
            .decrypt(ct, &aad)
            .expect("decrypt after rotate failed");
        assert_eq!(
            &recovered, plaintext,
            "value {i} must decrypt correctly after key rotation"
        );
    }
}

#[test]
fn rotate_all_keys_rewraps_under_new_kek() {
    let mut store = MemStore::default();
    let keyring = Keyring::new([0x11u8; 32]);
    let mut cipher = EnvelopeCipher::new(keyring);

    // Write 5 entries via the store using the raw cipher.
    let count = 5usize;
    for i in 0..count {
        let key = format!("k{i}").into_bytes();
        let val = format!("v{i}").into_bytes();
        let ct = cipher.encrypt(&val, &key).expect("encrypt failed");
        store.put(&key, &ct).expect("put failed");
    }

    // Rotate all keys to a new KEK.
    let rotated =
        rotate_all_keys(&mut store, &mut cipher, [0x22u8; 32]).expect("rotate_all_keys failed");
    assert_eq!(rotated, count as u64, "all entries should be rotated");

    // Active version should now be 2.
    assert_eq!(cipher.active_version().expect("version"), 2);

    // All entries must still decrypt.
    for i in 0..count {
        let key = format!("k{i}").into_bytes();
        let expected = format!("v{i}").into_bytes();
        let raw = store.get(&key).expect("get failed").expect("missing");
        let recovered = cipher
            .decrypt(&raw, &key)
            .expect("decrypt after rotate_all failed");
        assert_eq!(
            recovered, expected,
            "entry {i} mismatch after full rotation"
        );
    }
}

// ── Test 3: Wrong passphrase fails ────────────────────────────────────────────

#[test]
fn wrong_passphrase_different_kek_decryption_fails() {
    let salt = [0x55u8; 32];

    let ring_a = Keyring::from_passphrase_with_params(b"correct-horse", &salt, FAST_PARAMS)
        .expect("derive correct failed");
    let ring_b = Keyring::from_passphrase_with_params(b"wrong-password", &salt, FAST_PARAMS)
        .expect("derive wrong failed");

    // The two derived KEKs must be different.
    assert_ne!(
        ring_a.active_kek().expect("active_kek a"),
        ring_b.active_kek().expect("active_kek b"),
        "different passphrases must produce different KEKs"
    );

    let cipher_a = EnvelopeCipher::new(ring_a);
    let cipher_b = EnvelopeCipher::new(ring_b);

    let ct = cipher_a.encrypt(b"secret", b"").expect("encrypt failed");
    let result = cipher_b.decrypt(&ct, b"");

    assert!(
        result.is_err(),
        "decryption under wrong passphrase must fail"
    );
}

// ── Test 4: Passphrase round-trip ─────────────────────────────────────────────

#[test]
fn passphrase_round_trip() {
    let salt = [0x42u8; 32];
    let passphrase = b"a very secret passphrase";

    let ring1 = Keyring::from_passphrase_with_params(passphrase, &salt, FAST_PARAMS)
        .expect("first derivation failed");
    let cipher1 = EnvelopeCipher::new(ring1);

    let plaintext = b"data protected by passphrase";
    let ct = cipher1
        .encrypt(plaintext, b"test-aad")
        .expect("encrypt failed");

    // Re-derive the same KEK from the same passphrase + salt.
    let ring2 = Keyring::from_passphrase_with_params(passphrase, &salt, FAST_PARAMS)
        .expect("second derivation failed");
    let cipher2 = EnvelopeCipher::new(ring2);

    let recovered = cipher2.decrypt(&ct, b"test-aad").expect("decrypt failed");
    assert_eq!(recovered, plaintext);
}

#[test]
fn passphrase_derivation_is_deterministic() {
    let salt = [0x77u8; 32];
    let passphrase = b"deterministic passphrase";

    let ring1 = Keyring::from_passphrase_with_params(passphrase, &salt, FAST_PARAMS)
        .expect("derive 1 failed");
    let ring2 = Keyring::from_passphrase_with_params(passphrase, &salt, FAST_PARAMS)
        .expect("derive 2 failed");

    assert_eq!(
        ring1.active_kek().expect("active_kek ring1"),
        ring2.active_kek().expect("active_kek ring2"),
        "same passphrase+salt must produce same KEK"
    );
}

#[test]
fn generate_salt_returns_32_unique_bytes() {
    let s1 = generate_salt().expect("salt 1 failed");
    let s2 = generate_salt().expect("salt 2 failed");
    assert_eq!(s1.len(), 32);
    assert_ne!(s1, s2, "two generated salts should differ");
}

// ── Test 5: Version isolation ─────────────────────────────────────────────────

#[test]
fn v1_ciphertext_fails_with_v2_only_keyring() {
    // Encrypt under KEK v1.
    let kek_v1 = [0xAAu8; 32];
    let ring_v1 = Keyring::new(kek_v1);
    let cipher_v1 = EnvelopeCipher::new(ring_v1);

    let ct = cipher_v1
        .encrypt(b"secret-v1", b"")
        .expect("encrypt failed");

    // Build a fresh keyring that only knows KEK v2 (v1 is absent).
    let kek_v2 = [0xBBu8; 32];
    let ring_v2_only = Keyring::new(kek_v2);
    let cipher_v2_only = EnvelopeCipher::new(ring_v2_only);

    // Decryption must fail because v2-only keyring has no v1 entry.
    let result = cipher_v2_only.decrypt(&ct, b"");
    assert!(
        result.is_err(),
        "v1 ciphertext must not decrypt under v2-only keyring"
    );
}

#[test]
fn v1_ciphertext_decrypts_under_keyring_with_both_versions() {
    // Encrypt under v1.
    let kek_v1 = [0xAAu8; 32];
    let ring = Keyring::new(kek_v1);
    let cipher = EnvelopeCipher::new(ring);

    let ct = cipher.encrypt(b"secret", b"").expect("encrypt failed");

    // Add v2 to the same keyring (v1 is retained).
    let kek_v2 = [0xBBu8; 32];
    cipher
        .add_kek_version(kek_v2)
        .expect("add_kek_version failed");

    // v1 ciphertext must still decrypt because v1 is retained.
    let recovered = cipher
        .decrypt(&ct, b"")
        .expect("decrypt with both versions failed");
    assert_eq!(recovered, b"secret");
}

// ── Test 6: EncryptedKvEnvelope integration ───────────────────────────────────

#[test]
fn envelope_kv_put_get_round_trip() {
    let cipher = make_cipher();
    let store = EncryptedKvEnvelope::new(MemStore::default(), cipher);

    store.put(b"hello", b"world").expect("put failed");
    let got = store.get(b"hello").expect("get failed");
    assert_eq!(got, Some(b"world".to_vec()));
}

#[test]
fn envelope_kv_absent_key_returns_none() {
    let cipher = make_cipher();
    let store = EncryptedKvEnvelope::new(MemStore::default(), cipher);
    let got = store.get(b"nonexistent").expect("get failed");
    assert_eq!(got, None);
}

#[test]
fn envelope_kv_overwrite_returns_latest() {
    let cipher = make_cipher();
    let store = EncryptedKvEnvelope::new(MemStore::default(), cipher);
    store.put(b"k", b"v1").expect("put v1 failed");
    store.put(b"k", b"v2").expect("put v2 failed");
    let got = store.get(b"k").expect("get failed");
    assert_eq!(got, Some(b"v2".to_vec()));
}

#[test]
fn envelope_kv_delete_removes_entry() {
    let cipher = make_cipher();
    let store = EncryptedKvEnvelope::new(MemStore::default(), cipher);
    store.put(b"gone", b"value").expect("put failed");
    store.delete(b"gone").expect("delete failed");
    let got = store.get(b"gone").expect("get after delete failed");
    assert_eq!(got, None);
}

#[test]
fn envelope_kv_iter_decrypts_all_values() {
    let cipher = make_cipher();
    let store = EncryptedKvEnvelope::new(MemStore::default(), cipher);

    let pairs: &[(&[u8], &[u8])] = &[
        (b"alpha", b"AAA"),
        (b"beta", b"BBB"),
        (b"gamma", b"CCC"),
        (b"delta", b"DDD"),
        (b"epsilon", b"EEE"),
    ];

    for (k, v) in pairs {
        store.put(k, v).expect("put failed");
    }

    let mut iter_results: Vec<(Vec<u8>, Vec<u8>)> = store
        .iter()
        .expect("iter failed")
        .map(|item| item.expect("iter item failed"))
        .collect();
    iter_results.sort_by(|(a, _), (b, _)| a.cmp(b));

    assert_eq!(iter_results.len(), pairs.len());
    let mut expected: Vec<(&[u8], &[u8])> = pairs.to_vec();
    expected.sort_by_key(|(a, _)| *a);

    for ((got_k, got_v), (exp_k, exp_v)) in iter_results.iter().zip(expected.iter()) {
        assert_eq!(got_k.as_slice(), *exp_k);
        assert_eq!(got_v.as_slice(), *exp_v);
    }
}

#[test]
fn envelope_kv_multiple_keys_round_trip() {
    let cipher = make_cipher();
    let store = EncryptedKvEnvelope::new(MemStore::default(), cipher);

    for i in 0..20u32 {
        let key = format!("key-{i:04}").into_bytes();
        let val = format!("value-{i}").into_bytes();
        store.put(&key, &val).expect("put failed");
    }

    for i in 0..20u32 {
        let key = format!("key-{i:04}").into_bytes();
        let expected = format!("value-{i}").into_bytes();
        let got = store.get(&key).expect("get failed").expect("missing");
        assert_eq!(got, expected);
    }
}

#[test]
fn envelope_kv_rotate_kek_all_entries_still_readable() {
    let cipher = make_cipher();
    let mut store = EncryptedKvEnvelope::new(MemStore::default(), cipher);

    // Write 10 entries under v1.
    for i in 0..10u32 {
        let key = format!("rk{i}").into_bytes();
        let val = format!("rv{i}").into_bytes();
        store.put(&key, &val).expect("put failed");
    }

    // Rotate to v2.
    let rotated = store.rotate_kek([0xCCu8; 32]).expect("rotate_kek failed");
    assert_eq!(rotated, 10, "all 10 entries should be rotated");

    // All entries must still be readable.
    for i in 0..10u32 {
        let key = format!("rk{i}").into_bytes();
        let expected = format!("rv{i}").into_bytes();
        let got = store
            .get(&key)
            .expect("get after rotate failed")
            .expect("missing");
        assert_eq!(got, expected, "entry {i} mismatch after rotate_kek");
    }
}

// ── Test 7: Rotation count ────────────────────────────────────────────────────

#[test]
fn rotation_count_matches_entry_count() {
    let mut store = MemStore::default();
    let keyring = Keyring::new([0x99u8; 32]);
    let mut cipher = EnvelopeCipher::new(keyring);

    let entry_count = 7usize;
    for i in 0..entry_count {
        let key = format!("key-count-{i}").into_bytes();
        let val = format!("val-{i}").into_bytes();
        let ct = cipher.encrypt(&val, &key).expect("encrypt failed");
        store.put(&key, &ct).expect("put failed");
    }

    let count =
        rotate_all_keys(&mut store, &mut cipher, [0xFFu8; 32]).expect("rotate_all_keys failed");

    assert_eq!(
        count, entry_count as u64,
        "rotate_all_keys must return the exact count of rotated entries"
    );
}

#[test]
fn rotation_count_zero_for_empty_store() {
    let mut store = MemStore::default();
    let keyring = Keyring::new([0x01u8; 32]);
    let mut cipher = EnvelopeCipher::new(keyring);

    let count =
        rotate_all_keys(&mut store, &mut cipher, [0x02u8; 32]).expect("rotate on empty failed");
    assert_eq!(count, 0, "empty store should yield 0 rotated entries");
}

// ── Additional correctness tests ──────────────────────────────────────────────

#[test]
fn tampered_envelope_fails_authentication() {
    let cipher = make_cipher();
    let mut ct = cipher
        .encrypt(b"important data", b"aad")
        .expect("encrypt failed");

    // Flip a byte in the data ciphertext section (last 10 bytes contain data + tag).
    let last = ct.len() - 1;
    ct[last] ^= 0xFF;

    let result = cipher.decrypt(&ct, b"aad");
    assert!(
        result.is_err(),
        "tampered ciphertext must fail authentication"
    );
}

#[test]
fn wrong_aad_fails_authentication() {
    let cipher = make_cipher();
    let ct = cipher
        .encrypt(b"bound data", b"correct-aad")
        .expect("encrypt failed");

    let result = cipher.decrypt(&ct, b"wrong-aad");
    assert!(
        result.is_err(),
        "wrong AAD must cause authentication failure"
    );
}

#[test]
fn too_short_envelope_returns_error() {
    let cipher = make_cipher();
    let short = vec![0u8; 50]; // way too short
    let result = cipher.decrypt(&short, b"");
    assert!(result.is_err(), "too-short input must return an error");
}

#[test]
fn keyring_version_numbers_increment_monotonically() {
    let mut ring = Keyring::new([0x01u8; 32]);
    assert_eq!(ring.active_version().expect("version 1"), 1);
    ring.rotate([0x02u8; 32]).expect("rotate to v2");
    assert_eq!(ring.active_version().expect("version 2"), 2);
    ring.rotate([0x03u8; 32]).expect("rotate to v3");
    assert_eq!(ring.active_version().expect("version 3"), 3);
    let versions = ring.version_numbers();
    assert_eq!(versions, vec![1u32, 2, 3]);
}

#[test]
fn keyring_kek_for_version_lookup() {
    let kek1 = [0x11u8; 32];
    let kek2 = [0x22u8; 32];
    let mut ring = Keyring::new(kek1);
    ring.rotate(kek2).expect("rotate to kek2");

    assert_eq!(ring.kek_for_version(1), Some(&kek1));
    assert_eq!(ring.kek_for_version(2), Some(&kek2));
    assert_eq!(ring.kek_for_version(3), None);
}

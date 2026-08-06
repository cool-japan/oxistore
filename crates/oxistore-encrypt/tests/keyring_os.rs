//! Integration tests for the `os-keyring`-backed [`KeyringKey`] provider.
//!
//! These tests are gated behind the `os-keyring` feature and use the in-crate
//! `keyring_core::mock::Store` — an in-memory credential store — so they never
//! touch a real OS keyring.  The default test run (feature off) does not
//! compile this file, so CI without a keyring backend is unaffected.
//!
//! `keyring-core`'s default store is process-global, so we register the mock
//! store exactly once (via [`std::sync::Once`]) and give every test case a
//! unique label to keep them isolated from one another.

#![cfg(feature = "os-keyring")]

use oxistore_encrypt::{EncryptError, KeyProvider, KeyringKey};
use std::sync::Once;

static INIT: Once = Once::new();

/// Register the in-memory mock credential store exactly once for the whole
/// test binary.
fn init_mock_store() {
    INIT.call_once(|| {
        let store = keyring_core::mock::Store::new().expect("build mock credential store");
        keyring_core::set_default_store(store);
    });
}

/// A round-trip: storing a 32-byte key and reading it back yields exactly the
/// original bytes.
#[test]
fn store_then_get_returns_exact_32_bytes() {
    init_mock_store();
    let key = KeyringKey::new("roundtrip-label");
    let secret = [0xABu8; 32];

    key.store_key(&secret).expect("store_key");

    let got = key.get_key().expect("get_key after store");
    assert_eq!(got.len(), 32, "provider must return exactly 32 bytes");
    assert_eq!(got, &secret, "returned bytes must match stored key");
}

/// After deleting the entry, a *fresh* provider must fail with
/// `KeyringUnavailable` (the mock maps missing entries to `NoEntry`).
#[test]
fn delete_then_get_is_keyring_unavailable() {
    init_mock_store();
    let label = "delete-label";

    // Store then delete.
    let writer = KeyringKey::new(label);
    writer.store_key(&[0x11u8; 32]).expect("store_key");
    writer.delete_entry().expect("delete_entry");

    // A fresh provider (no cached bytes) must now fail to load.
    let reader = KeyringKey::new(label);
    match reader.get_key() {
        Err(EncryptError::KeyringUnavailable { label: l }) => {
            assert!(
                l.contains("delete-label"),
                "error should name the label: {l}"
            );
        }
        other => panic!("expected KeyringUnavailable after delete, got {other:?}"),
    }
}

/// `delete_entry` on an absent label is a no-op (does not error).
#[test]
fn delete_absent_entry_is_ok() {
    init_mock_store();
    let key = KeyringKey::new("never-stored-label");
    key.delete_entry()
        .expect("delete_entry on absent label must be Ok");
}

/// A stored value that is not a valid 64-char hex string yields
/// `InvalidKeyLength`.
#[test]
fn non_hex_secret_is_invalid_key_length() {
    init_mock_store();
    let label = "bad-hex-label";

    // Write a bogus (too-short, non-hex) secret directly via keyring-core.
    let entry = keyring_core::Entry::new("oxistore", label).expect("entry");
    entry
        .set_password("not-a-valid-hex-key")
        .expect("set_password");

    let key = KeyringKey::new(label);
    match key.get_key() {
        Err(EncryptError::InvalidKeyLength { .. }) => {}
        other => panic!("expected InvalidKeyLength for non-hex secret, got {other:?}"),
    }
}

/// A hex string of the wrong length (not 64 chars) yields `InvalidKeyLength`.
#[test]
fn short_hex_secret_is_invalid_key_length() {
    init_mock_store();
    let label = "short-hex-label";

    // 32 hex chars = 16 bytes, not 32.
    let entry = keyring_core::Entry::new("oxistore", label).expect("entry");
    entry
        .set_password("00112233445566778899aabbccddeeff")
        .expect("set_password");

    let key = KeyringKey::new(label);
    match key.get_key() {
        Err(EncryptError::InvalidKeyLength { got }) => {
            assert_eq!(got, 16, "16-byte key should report got=16");
        }
        other => panic!("expected InvalidKeyLength for short hex, got {other:?}"),
    }
}

/// The `OnceLock` cache means the first successful load is reused: mutating the
/// underlying store afterwards does not change what an already-loaded provider
/// returns.
#[test]
fn get_key_caches_first_load() {
    init_mock_store();
    let label = "cache-label";

    let key = KeyringKey::new(label);
    key.store_key(&[0x22u8; 32]).expect("store_key");

    // Prime the cache.
    let first = key.get_key().expect("first get_key").to_vec();
    assert_eq!(first, vec![0x22u8; 32]);

    // Delete the backing entry; the cached provider must still return the
    // original bytes because it never re-queries after the first success.
    key.delete_entry().expect("delete_entry");
    let second = key.get_key().expect("cached get_key must still succeed");
    assert_eq!(second, first.as_slice(), "cached value must be stable");
}

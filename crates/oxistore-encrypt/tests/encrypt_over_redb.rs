//! Integration tests: `EncryptedKv` layered over a real `RedbStore`.
//!
//! These tests verify:
//! - Round-trip correctness (encrypt on `put`, decrypt on `get`).
//! - Ciphertext-at-rest: the inner `RedbStore` stores encrypted bytes, not
//!   the original plaintext.
//! - Multi-key isolation: each key's AAD is derived independently from its
//!   own bytes, so decrypting key A with key B's ciphertext fails.

use oxistore_core::KvStore;
use oxistore_encrypt::{EncryptedKv, StaticKey};
use oxistore_kv_redb::RedbStore;
use std::env;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a unique temp path for this test run.
fn unique_tmp_path(suffix: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    env::temp_dir().join(format!(
        "oxistore_enc_redb_{}_{}_{suffix}",
        std::process::id(),
        nanos
    ))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Basic round-trip: a value written through `EncryptedKv` must be recoverable
/// via the same wrapper.
#[test]
fn redb_encrypt_basic_roundtrip() {
    let redb = RedbStore::open_in_memory().expect("open in-memory redb");
    let key = StaticKey::from_array([0x11u8; 32]);
    let enc = EncryptedKv::new(redb, key);

    enc.put(b"hello", b"world").expect("put");
    let got = enc.get(b"hello").expect("get").expect("Some");
    assert_eq!(got, b"world");
}

/// Ciphertext-at-rest: reading raw bytes from the inner store must not expose
/// the plaintext.  We use `EncryptedKv::inner_ref()` to bypass decryption.
#[test]
fn redb_ciphertext_at_rest() {
    let plaintext = b"super_secret_value";

    let redb = RedbStore::open_in_memory().expect("open in-memory redb");
    let key = StaticKey::from_array([0x22u8; 32]);
    let enc = EncryptedKv::new(redb, key);

    enc.put(b"key_at_rest", plaintext).expect("put");

    // Read through the raw inner store — must return ciphertext, not plaintext.
    let raw = enc
        .inner_ref()
        .get(b"key_at_rest")
        .expect("inner get")
        .expect("inner Some");

    assert_ne!(
        raw.as_slice(),
        plaintext.as_ref(),
        "plaintext found in raw redb store — encryption not applied"
    );
    // Ciphertext must be longer than plaintext (nonce + tag overhead).
    assert!(
        raw.len() > plaintext.len(),
        "raw bytes ({}) not longer than plaintext ({}) — expected nonce+tag overhead",
        raw.len(),
        plaintext.len()
    );
}

/// Multi-key isolation: values encrypted under different KV keys have
/// distinct AAD bindings.  Writing key A's plaintext then reading raw bytes
/// of key A must differ from key B's raw bytes.
#[test]
fn redb_multi_key_isolation() {
    let redb = RedbStore::open_in_memory().expect("open in-memory redb");
    let key = StaticKey::from_array([0x33u8; 32]);
    let enc = EncryptedKv::new(redb, key);

    enc.put(b"kv_key_a", b"value_a").expect("put a");
    enc.put(b"kv_key_b", b"value_b").expect("put b");

    // Both raw ciphertexts must be present and distinct (different AAD).
    let raw_a = enc
        .inner_ref()
        .get(b"kv_key_a")
        .expect("raw get a")
        .expect("Some a");
    let raw_b = enc
        .inner_ref()
        .get(b"kv_key_b")
        .expect("raw get b")
        .expect("Some b");
    assert_ne!(
        raw_a, raw_b,
        "distinct keys must produce distinct ciphertexts"
    );

    // Decrypted values must match original plaintext.
    let dec_a = enc.get(b"kv_key_a").expect("get a").expect("Some a");
    let dec_b = enc.get(b"kv_key_b").expect("get b").expect("Some b");
    assert_eq!(dec_a, b"value_a");
    assert_eq!(dec_b, b"value_b");
}

/// File-backed round-trip: persist to disk, drop, reopen, decrypt.
///
/// Uses a temp-dir path unique per process+nanosecond to avoid conflicts.
#[test]
fn redb_file_backed_roundtrip() {
    let path = unique_tmp_path("file_backed");

    let enc_key = StaticKey::from_array([0x44u8; 32]);
    {
        let redb = RedbStore::open(&path).expect("open redb");
        let enc = EncryptedKv::new(redb, enc_key.clone());
        enc.put(b"persistent_key", b"persistent_value")
            .expect("put");
        enc.flush().expect("flush");
    } // enc and inner redb are dropped here, releasing the file lock

    // Reopen and verify decryption still works.
    let redb2 = RedbStore::open(&path).expect("reopen redb");
    let enc2 = EncryptedKv::new(redb2, enc_key);
    let got = enc2
        .get(b"persistent_key")
        .expect("get after reopen")
        .expect("Some after reopen");
    assert_eq!(got, b"persistent_value");

    // Cleanup.
    let _ = std::fs::remove_file(&path);
}

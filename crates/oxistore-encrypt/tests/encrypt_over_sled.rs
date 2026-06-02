//! Integration tests: `EncryptedKv` layered over a real `SledStore`.
//!
//! Mirrors the Redb integration tests using sled as the backend.  Sled's
//! `open_temporary()` constructor provides an ephemeral, self-cleaning store
//! that is ideal for test isolation.

use oxistore_core::KvStore;
use oxistore_encrypt::{EncryptedKv, StaticKey};
use oxistore_kv_sled::SledStore;
use std::env;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a unique temp path for file-backed sled tests.
fn unique_tmp_path(suffix: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    env::temp_dir().join(format!(
        "oxistore_enc_sled_{}_{}_{suffix}",
        std::process::id(),
        nanos
    ))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Basic round-trip using sled's ephemeral (temporary) backend.
#[test]
fn sled_encrypt_basic_roundtrip() {
    let sled = SledStore::open_temporary().expect("open temporary sled");
    let key = StaticKey::from_array([0xAAu8; 32]);
    let enc = EncryptedKv::new(sled, key);

    enc.put(b"hello", b"world").expect("put");
    let got = enc.get(b"hello").expect("get").expect("Some");
    assert_eq!(got, b"world");
}

/// Ciphertext-at-rest: the inner sled store must hold ciphertext, not plaintext.
#[test]
fn sled_ciphertext_at_rest() {
    let plaintext = b"very_secret_payload";

    let sled = SledStore::open_temporary().expect("open temporary sled");
    let key = StaticKey::from_array([0xBBu8; 32]);
    let enc = EncryptedKv::new(sled, key);

    enc.put(b"secret_key", plaintext).expect("put");

    // Read raw bytes via the inner store — must not be plaintext.
    let raw = enc
        .inner_ref()
        .get(b"secret_key")
        .expect("inner get")
        .expect("inner Some");

    assert_ne!(
        raw.as_slice(),
        plaintext.as_ref(),
        "plaintext found in raw sled store — encryption not applied"
    );
    assert!(
        raw.len() > plaintext.len(),
        "raw bytes ({}) not longer than plaintext ({}) — expected nonce+tag overhead",
        raw.len(),
        plaintext.len()
    );
}

/// Multi-key isolation: distinct KV keys produce distinct ciphertexts (AAD
/// binds ciphertext to its storage location).
#[test]
fn sled_multi_key_isolation() {
    let sled = SledStore::open_temporary().expect("open temporary sled");
    let key = StaticKey::from_array([0xCCu8; 32]);
    let enc = EncryptedKv::new(sled, key);

    enc.put(b"alpha", b"value_alpha").expect("put alpha");
    enc.put(b"beta", b"value_beta").expect("put beta");

    let raw_alpha = enc
        .inner_ref()
        .get(b"alpha")
        .expect("raw get alpha")
        .expect("Some alpha");
    let raw_beta = enc
        .inner_ref()
        .get(b"beta")
        .expect("raw get beta")
        .expect("Some beta");
    assert_ne!(
        raw_alpha, raw_beta,
        "distinct keys must yield distinct ciphertexts"
    );

    let dec_alpha = enc.get(b"alpha").expect("get alpha").expect("Some alpha");
    let dec_beta = enc.get(b"beta").expect("get beta").expect("Some beta");
    assert_eq!(dec_alpha, b"value_alpha");
    assert_eq!(dec_beta, b"value_beta");
}

/// File-backed sled round-trip: data survives a drop+reopen cycle.
#[test]
fn sled_file_backed_roundtrip() {
    let path = unique_tmp_path("file_backed");

    let enc_key = StaticKey::from_array([0xDDu8; 32]);
    {
        let sled = SledStore::open(&path).expect("open sled");
        let enc = EncryptedKv::new(sled, enc_key.clone());
        enc.put(b"durable_key", b"durable_value").expect("put");
        enc.flush().expect("flush");
    }

    let sled2 = SledStore::open(&path).expect("reopen sled");
    let enc2 = EncryptedKv::new(sled2, enc_key);
    let got = enc2
        .get(b"durable_key")
        .expect("get after reopen")
        .expect("Some after reopen");
    assert_eq!(got, b"durable_value");

    // Cleanup.
    let _ = std::fs::remove_dir_all(&path);
}

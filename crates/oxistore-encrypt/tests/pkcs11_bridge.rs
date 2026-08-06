//! Integration test for the PKCS#11 HSM-backed [`Pkcs11KeyProvider`] bridge.
//!
//! This test is gated behind the `oxicrypto-pkcs11` feature **and** marked
//! `#[ignore]`, because it requires a live PKCS#11 token (e.g. SoftHSM2).  It is
//! skipped in the default CI run.
//!
//! To run it locally against SoftHSM2:
//!
//! ```sh
//! export OXISTORE_PKCS11_MODULE=/usr/lib/softhsm/libsofthsm2.so
//! export OXISTORE_PKCS11_PIN=1234
//! export OXISTORE_PKCS11_SLOT=0   # optional index into the token slot list
//! cargo test -p oxistore-encrypt --features oxicrypto-pkcs11 -- --ignored pkcs11
//! ```

#![cfg(feature = "oxicrypto-pkcs11")]

use std::sync::Arc;

use oxicrypto_adapter_pkcs11::Pkcs11Provider;
use oxistore_core::KvStore;
use oxistore_encrypt::{EncryptedKv, KeyProvider, Pkcs11KeyProvider};
use oxistore_kv_redb::RedbStore;

/// Read the module path from the environment, or return `None` to skip.
fn module_path() -> Option<std::path::PathBuf> {
    std::env::var("OXISTORE_PKCS11_MODULE")
        .ok()
        .map(std::path::PathBuf::from)
}

/// Full round-trip: generate an extractable AES-256 key on the token, bridge it
/// through [`Pkcs11KeyProvider`], and encrypt/decrypt a value through
/// [`EncryptedKv`].
#[test]
#[ignore = "requires a live PKCS#11 token (SoftHSM2); set OXISTORE_PKCS11_* env vars"]
fn pkcs11_extractable_key_round_trips_through_encrypted_kv() {
    let Some(module) = module_path() else {
        eprintln!("OXISTORE_PKCS11_MODULE not set; skipping PKCS#11 integration test");
        return;
    };
    let pin = std::env::var("OXISTORE_PKCS11_PIN").unwrap_or_else(|_| "1234".to_string());
    let slot_index: usize = std::env::var("OXISTORE_PKCS11_SLOT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Discover slots via the adapter (avoids importing cryptoki types directly).
    let slots = Pkcs11Provider::list_slots(&module).expect("list_slots");
    // `Slot` is `Copy`; `TokenInfo` is not, so borrow the tuple and copy the slot.
    let slot = slots
        .get(slot_index)
        .map(|(s, _info)| *s)
        .expect("requested PKCS#11 slot index out of range");

    let provider = Pkcs11Provider::new(&module, slot, &pin).expect("open PKCS#11 session");

    // Unique label so repeated runs do not collide.
    let label = format!(
        "oxistore-test-key-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    provider
        .generate_extractable_aes_key(&label)
        .expect("generate extractable AES key");

    let provider = Arc::new(provider);

    // Sanity: the bridge returns exactly 32 bytes.
    let key = Pkcs11KeyProvider::new(Arc::clone(&provider), label.clone());
    let raw = key.get_key().expect("bridge get_key");
    assert_eq!(raw.len(), 32, "AES-256 key must be 32 bytes");

    // Full encryption round-trip over a real redb backend.
    let redb = RedbStore::open_in_memory().expect("open redb");
    let enc = EncryptedKv::new(redb, key);
    enc.put(b"pkcs11-key", b"secret-payload").expect("put");
    assert_eq!(
        enc.get(b"pkcs11-key").expect("get"),
        Some(b"secret-payload".to_vec()),
    );
}

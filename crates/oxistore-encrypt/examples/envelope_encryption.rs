//! Envelope encryption: transparent per-value AEAD, key rotation, and
//! transaction/snapshot support over a real backend.
//!
//! ```sh
//! cargo run -p oxistore-encrypt --example envelope_encryption
//! ```
//!
//! Each value is encrypted with its own random Data Encryption Key (DEK),
//! which is itself wrapped under the active Key Encrypting Key (KEK) held in
//! a [`Keyring`]. Rotating the KEK only re-wraps the small DEK wrapper for
//! every entry — the bulk ciphertext is never re-encrypted.

use oxistore_core::KvStore;
use oxistore_encrypt::{rotate_all_keys, EncryptedKvEnvelope, EnvelopeCipher, Keyring};
use oxistore_kv_redb::RedbStore;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Set up an envelope cipher over an in-memory redb backend ──────────
    let keyring = Keyring::new([0x01u8; 32]); // KEK version 1
    let cipher = EnvelopeCipher::new(keyring);
    let redb = RedbStore::open_in_memory()?;
    let mut envelope = EncryptedKvEnvelope::new(redb, cipher);

    // ── Transparent put/get: the caller never sees ciphertext ────────────
    envelope.put(b"account:alice", b"balance=1000")?;
    envelope.put(b"account:bob", b"balance=500")?;
    println!(
        "get(account:alice) -> {:?}",
        envelope
            .get(b"account:alice")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    // ── EnvelopeTxn: transactions decrypt/encrypt transparently too ──────
    // Obtained via the KvStore::transaction() trait method; commit/rollback
    // delegate to the inner backend's real transaction, so atomicity comes
    // from redb, not from this layer.
    {
        let mut txn = envelope.transaction()?;
        txn.put(b"account:carol", b"balance=250")?;
        // Read-your-writes: the just-written value decrypts correctly
        // within the same transaction, before it's committed.
        println!(
            "inside txn, get(account:carol) -> {:?}",
            txn.get(b"account:carol")?
                .map(|v| String::from_utf8_lossy(&v).into_owned())
        );
        txn.commit()?;
    }
    println!(
        "after commit, get(account:carol) -> {:?}",
        envelope
            .get(b"account:carol")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    // ── EnvelopeSnapshot: point-in-time view, also transparently decrypted ─
    let snapshot_before = envelope.snapshot()?;
    envelope.put(b"account:alice", b"balance=750")?; // spent 250 after the snapshot
    println!(
        "live store now sees   account:alice = {:?}",
        envelope
            .get(b"account:alice")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );
    println!(
        "snapshot still sees   account:alice = {:?}",
        snapshot_before
            .get(b"account:alice")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );
    // Explicitly drop the snapshot to release its borrow of `envelope` before
    // taking a `&mut` reference below (a `Box<dyn KvSnapshot>`'s destructor
    // is opaque to the borrow checker, so it otherwise keeps the borrow
    // alive until the end of scope regardless of last use).
    drop(snapshot_before);

    // ── Key rotation via the decorator: EncryptedKvEnvelope::rotate_kek ───
    // Re-wraps every entry's DEK under a fresh KEK (version 2) without
    // touching the (potentially much larger) data ciphertext.
    let rotated = envelope.rotate_kek([0x02u8; 32])?;
    println!("rotate_kek -> re-wrapped {rotated} entries to KEK version 2");
    println!(
        "post-rotation, get(account:bob) still decrypts -> {:?}",
        envelope
            .get(b"account:bob")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    // ── Key rotation via the free function: rotate_all_keys(&mut store, ..) ─
    // Same operation, expressed as a standalone function over a raw
    // KvStore + EnvelopeCipher pair instead of the EncryptedKvEnvelope
    // decorator — useful for an offline/batch rotation job that doesn't
    // want to construct the full decorator.
    let mut raw_store = RedbStore::open_in_memory()?;
    let mut raw_cipher = EnvelopeCipher::new(Keyring::new([0xAAu8; 32]));
    for i in 0..5u32 {
        let ct = raw_cipher.encrypt(
            format!("value-{i}").as_bytes(),
            format!("key-{i}").as_bytes(),
        )?;
        raw_store.put(format!("key-{i}").as_bytes(), &ct)?;
    }
    let count = rotate_all_keys(&mut raw_store, &mut raw_cipher, [0xBBu8; 32])?;
    println!("rotate_all_keys (free function) -> rotated {count} entries");
    let ct = raw_store
        .get(b"key-3")?
        .ok_or("key-3 unexpectedly absent after rotate_all_keys")?;
    let pt = raw_cipher.decrypt(&ct, b"key-3")?;
    println!(
        "post-rotation, direct decrypt of key-3 -> {:?}",
        String::from_utf8_lossy(&pt)
    );

    Ok(())
}

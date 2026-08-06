//! Integration tests for envelope-encrypted transactions and snapshots.
//!
//! These are backed by a real `RedbStore` (which provides genuine buffered
//! transactions and MVCC snapshots), so they exercise the full
//! [`EnvelopeTxn`](oxistore_encrypt::EnvelopeTxn) /
//! [`EnvelopeSnapshot`](oxistore_encrypt::EnvelopeSnapshot) decryption path over
//! a real backend.

use oxistore_core::KvStore;
use oxistore_encrypt::{EncryptedKvEnvelope, EnvelopeCipher, Keyring, MIN_ENVELOPE_LEN};
use oxistore_kv_redb::RedbStore;

fn cipher() -> EnvelopeCipher {
    EnvelopeCipher::new(Keyring::new([0x5Au8; 32]))
}

/// Build an envelope store over a fresh in-memory redb backend.
fn store() -> EncryptedKvEnvelope<RedbStore> {
    let redb = RedbStore::open_in_memory().expect("open in-memory redb");
    EncryptedKvEnvelope::new(redb, cipher())
}

/// Read-your-writes inside a transaction, then commit and verify durability.
#[test]
fn txn_read_your_writes_and_commit() {
    let enc = store();

    {
        let mut txn = enc.transaction().expect("begin txn");
        txn.put(b"alpha", b"one").expect("put alpha");
        txn.put(b"beta", b"two").expect("put beta");

        // Read-your-writes: values are visible (and decrypted) within the txn.
        assert_eq!(txn.get(b"alpha").expect("get alpha"), Some(b"one".to_vec()));
        assert_eq!(txn.get(b"beta").expect("get beta"), Some(b"two".to_vec()));
        assert!(txn.contains(b"alpha").expect("contains"));

        txn.commit().expect("commit");
    }

    // After commit the store decrypts committed values transparently.
    assert_eq!(enc.get(b"alpha").expect("get alpha"), Some(b"one".to_vec()));
    assert_eq!(enc.get(b"beta").expect("get beta"), Some(b"two".to_vec()));
}

/// Rolled-back transactions discard their writes.
#[test]
fn txn_rollback_discards() {
    let enc = store();
    enc.put(b"keep", b"v").expect("seed put");

    {
        let mut txn = enc.transaction().expect("begin txn");
        txn.put(b"ephemeral", b"x").expect("put ephemeral");
        assert_eq!(
            txn.get(b"ephemeral").expect("get in txn"),
            Some(b"x".to_vec())
        );
        txn.rollback().expect("rollback");
    }

    assert_eq!(enc.get(b"ephemeral").expect("get after rollback"), None);
    assert_eq!(enc.get(b"keep").expect("get keep"), Some(b"v".to_vec()));
}

/// A snapshot is an immutable point-in-time view: writes made after it are
/// invisible, and reads through it are decrypted.
#[test]
fn snapshot_isolation_and_decrypt() {
    let enc = store();
    enc.put(b"k1", b"v1").expect("put k1");

    let snap = enc.snapshot().expect("snapshot");
    assert_eq!(snap.get(b"k1").expect("snap get k1"), Some(b"v1".to_vec()));

    // Mutate the live store after taking the snapshot.
    enc.put(b"k2", b"v2").expect("put k2");
    enc.put(b"k1", b"changed").expect("overwrite k1");

    // Snapshot still reflects the captured state.
    assert_eq!(
        snap.get(b"k1").expect("snap get k1 again"),
        Some(b"v1".to_vec()),
        "snapshot must not see post-snapshot overwrite"
    );
    assert_eq!(
        snap.get(b"k2").expect("snap get k2"),
        None,
        "snapshot must not see post-snapshot insert"
    );
    assert!(!snap.contains(b"k2").expect("snap contains k2"));

    // The live store, however, reflects the mutations.
    assert_eq!(enc.get(b"k1").expect("live k1"), Some(b"changed".to_vec()));
    assert_eq!(enc.get(b"k2").expect("live k2"), Some(b"v2".to_vec()));
}

/// Range scans over both txn and snapshot must decrypt every value.
#[test]
fn txn_and_snapshot_range_decrypt() {
    let enc = store();
    enc.put(b"p:a", b"A").expect("put a");
    enc.put(b"p:b", b"B").expect("put b");
    enc.put(b"q:c", b"C").expect("put c");

    // Snapshot range over the "p:" prefix.
    let snap = enc.snapshot().expect("snapshot");
    let mut got: Vec<(Vec<u8>, Vec<u8>)> = snap
        .range(b"p:", b"p;")
        .expect("snap range")
        .map(|r| r.expect("snap item"))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (b"p:a".to_vec(), b"A".to_vec()),
            (b"p:b".to_vec(), b"B".to_vec())
        ]
    );

    // Transaction range should also decrypt (committed + buffered).
    let mut txn = enc.transaction().expect("txn");
    txn.put(b"p:d", b"D").expect("put d in txn");
    let mut tgot: Vec<(Vec<u8>, Vec<u8>)> = txn
        .range(b"p:", b"p;")
        .expect("txn range")
        .map(|r| r.expect("txn item"))
        .collect();
    tgot.sort();
    assert_eq!(
        tgot,
        vec![
            (b"p:a".to_vec(), b"A".to_vec()),
            (b"p:b".to_vec(), b"B".to_vec()),
            (b"p:d".to_vec(), b"D".to_vec()),
        ]
    );
    txn.rollback().expect("rollback");
}

/// The raw bytes persisted by a committed envelope transaction must be genuine
/// envelope ciphertext (not plaintext), proving encryption actually happened.
#[test]
fn txn_persists_ciphertext_not_plaintext() {
    // Keep a clone of the raw backend so we can inspect the bytes at rest.
    let redb = RedbStore::open_in_memory().expect("open redb");
    let raw = redb.clone();
    let enc = EncryptedKvEnvelope::new(redb, cipher());

    {
        let mut txn = enc.transaction().expect("txn");
        txn.put(b"secret", b"plaintext-value").expect("put");
        txn.commit().expect("commit");
    }

    // Read the raw stored bytes directly from the underlying redb store.
    let at_rest = raw
        .get(b"secret")
        .expect("raw get")
        .expect("value present at rest");
    assert!(
        at_rest.len() >= MIN_ENVELOPE_LEN,
        "stored value must be a full envelope (>= {MIN_ENVELOPE_LEN} bytes), got {}",
        at_rest.len()
    );
    assert_ne!(
        at_rest, b"plaintext-value",
        "value at rest must be ciphertext, not plaintext"
    );
    assert!(
        !at_rest
            .windows(b"plaintext-value".len())
            .any(|w| w == b"plaintext-value"),
        "plaintext must not appear anywhere in the stored ciphertext"
    );

    // And the value still decrypts through the store.
    assert_eq!(
        enc.get(b"secret").expect("get"),
        Some(b"plaintext-value".to_vec())
    );
}

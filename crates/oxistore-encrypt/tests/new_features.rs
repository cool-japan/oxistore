//! Tests for Slice 5 new features in `oxistore-encrypt`.
//!
//! Coverage:
//! - BLAKE3 cell ID derivation
//! - AES-256-GCM-SIV round trip
//! - Wrong key decryption failure
//! - Transplant attack rejection via BLAKE3 AAD
//! - `EncryptedTxn`: put→commit→get; rollback discards
//! - `EncryptedSnapshot`: point-in-time isolation
//! - `CipherBuilder`: both AEAD types
//! - XChaCha20-Poly1305 (existing path) round trip with new generics

use std::collections::HashMap;
use std::sync::Mutex;

use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
use oxistore_encrypt::{
    derive_cell_id, AeadChoice, AeadKind, AesGcmSiv256Aead, CipherBuilder, EncryptedKv, StaticKey,
    XChaCha20Poly1305Aead,
};

// ── Full in-memory KvStore with txn + snapshot support ───────────────────────

/// A simple in-memory KV store that supports transactions and snapshots.
#[derive(Default)]
struct MemStore {
    data: Mutex<HashMap<Vec<u8>, Vec<u8>>>,
}

impl KvStore for MemStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.data.lock().expect("lock").get(key).cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.data
            .lock()
            .expect("lock")
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.data.lock().expect("lock").remove(key);
        Ok(())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let guard = self.data.lock().expect("lock");
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
        let snap: HashMap<Vec<u8>, Vec<u8>> = self.data.lock().expect("lock").clone();
        Ok(Box::new(MemTxn {
            store: self,
            buf: snap,
            deleted: Vec::new(),
        }))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        let snap: HashMap<Vec<u8>, Vec<u8>> = self.data.lock().expect("lock").clone();
        Ok(Box::new(MemSnap(snap)))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let guard = self.data.lock().expect("lock");
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

// ── MemTxn ────────────────────────────────────────────────────────────────────

struct MemTxn<'a> {
    store: &'a MemStore,
    buf: HashMap<Vec<u8>, Vec<u8>>,
    deleted: Vec<Vec<u8>>,
}

impl<'a> KvTxn for MemTxn<'a> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        if self.deleted.iter().any(|k| k == key) {
            return Ok(None);
        }
        Ok(self.buf.get(key).cloned())
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.deleted.retain(|k| k != key);
        self.buf.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StoreError> {
        self.buf.remove(key);
        self.deleted.push(key.to_vec());
        Ok(())
    }

    fn range<'s>(&'s self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'s>, StoreError> {
        let lo = lo.to_vec();
        let hi = hi.to_vec();
        let pairs: Vec<_> = self
            .buf
            .iter()
            .filter(|(k, _)| **k >= lo && **k < hi && !self.deleted.iter().any(|d| d == *k))
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn commit(self: Box<Self>) -> Result<(), StoreError> {
        let mut data = self.store.data.lock().expect("lock");
        for k in &self.deleted {
            data.remove(k);
        }
        for (k, v) in &self.buf {
            if !self.deleted.iter().any(|d| d == k) {
                data.insert(k.clone(), v.clone());
            }
        }
        Ok(())
    }

    fn rollback(self: Box<Self>) -> Result<(), StoreError> {
        // Discard changes — nothing to do.
        Ok(())
    }
}

// ── MemSnap ───────────────────────────────────────────────────────────────────

struct MemSnap(HashMap<Vec<u8>, Vec<u8>>);

impl KvSnapshot for MemSnap {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.0.get(key).cloned())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let lo = lo.to_vec();
        let hi = hi.to_vec();
        let pairs: Vec<_> = self
            .0
            .iter()
            .filter(|(k, _)| **k >= lo && **k < hi)
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn key_a() -> [u8; 32] {
    [0x11u8; 32]
}
fn key_b() -> [u8; 32] {
    [0x22u8; 32]
}

fn enc_xchacha() -> EncryptedKv<MemStore, StaticKey, XChaCha20Poly1305Aead> {
    EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        XChaCha20Poly1305Aead,
    )
}

fn enc_aes_gcm_siv() -> EncryptedKv<MemStore, StaticKey, AesGcmSiv256Aead> {
    EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        AesGcmSiv256Aead,
    )
}

// ── BLAKE3 cell ID tests ──────────────────────────────────────────────────────

#[test]
fn blake3_cell_id_same_key_same_id() {
    let id1 = derive_cell_id(b"my-key");
    let id2 = derive_cell_id(b"my-key");
    assert_eq!(id1, id2, "same input must produce same cell ID");
}

#[test]
fn blake3_cell_id_different_key_different_id() {
    let id_a = derive_cell_id(b"key-alpha");
    let id_b = derive_cell_id(b"key-beta");
    assert_ne!(
        id_a, id_b,
        "different inputs must produce different cell IDs"
    );
}

#[test]
fn blake3_cell_id_is_32_bytes() {
    let id = derive_cell_id(b"anything");
    assert_eq!(id.len(), 32);
}

// ── AES-256-GCM-SIV round trip ───────────────────────────────────────────────

#[test]
fn aes_gcm_siv_basic_put_get_round_trip() {
    let enc = enc_aes_gcm_siv();
    enc.put(b"hello", b"world").expect("put failed");
    let result = enc.get(b"hello").expect("get failed");
    assert_eq!(result, Some(b"world".to_vec()));
}

#[test]
fn aes_gcm_siv_absent_key_returns_none() {
    let enc = enc_aes_gcm_siv();
    assert_eq!(enc.get(b"absent").expect("get failed"), None);
}

#[test]
fn aes_gcm_siv_empty_plaintext_round_trips() {
    let enc = enc_aes_gcm_siv();
    enc.put(b"e", b"").expect("put failed");
    let result = enc.get(b"e").expect("get failed");
    assert_eq!(result, Some(vec![]));
}

#[test]
fn aes_gcm_siv_multiple_keys_round_trip() {
    let enc = enc_aes_gcm_siv();
    let pairs: &[(&[u8], &[u8])] = &[(b"alpha", b"AAA"), (b"beta", b"BBB"), (b"gamma", b"CCC")];
    for (k, v) in pairs {
        enc.put(k, v).expect("put failed");
    }
    for (k, v) in pairs {
        let result = enc.get(k).expect("get failed");
        assert_eq!(result.as_deref(), Some(*v));
    }
}

// ── Wrong-key decryption failure ──────────────────────────────────────────────

#[test]
fn wrong_key_xchacha20_fails_decryption() {
    // Encrypt with key_a, then try to decrypt with key_b using raw AEAD.
    use oxistore_encrypt::aead::{decrypt_with_aead, encrypt_with_aead};

    let cipher = XChaCha20Poly1305Aead;
    let aad = derive_cell_id(b"data");

    let ct = encrypt_with_aead(&cipher, &key_a(), &aad, b"secret").expect("encrypt");

    // Must fail with a different key.
    let result = decrypt_with_aead(&cipher, &key_b(), &aad, &ct);
    assert!(
        result.is_err(),
        "decryption with wrong key must fail, got: {result:?}"
    );
}

#[test]
fn wrong_key_aes_gcm_siv_fails_decryption() {
    use oxistore_encrypt::aead::{decrypt_with_aead, encrypt_with_aead};

    let cipher = AesGcmSiv256Aead;
    let aad = derive_cell_id(b"data");

    let ct = encrypt_with_aead(&cipher, &key_a(), &aad, b"secret").expect("encrypt");

    let result = decrypt_with_aead(&cipher, &key_b(), &aad, &ct);
    assert!(result.is_err(), "decryption with wrong key must fail");
}

// ── Transplant attack prevention ──────────────────────────────────────────────

/// Low-level AAD test: verifies the AEAD mechanism blocks cross-key decryption.
#[test]
fn transplant_attack_aad_level() {
    use oxistore_encrypt::aead::{decrypt_with_aead, encrypt_with_aead};

    let cipher = XChaCha20Poly1305Aead;
    let key = key_a();

    // Encrypt a value for "src-key" (AAD = BLAKE3("src-key")).
    let aad_src = derive_cell_id(b"src-key");
    let ct = encrypt_with_aead(&cipher, &key, &aad_src, b"value").expect("encrypt");

    // Now attempt to decrypt with AAD for "dst-key" — must fail.
    let aad_dst = derive_cell_id(b"dst-key");
    let result = decrypt_with_aead(&cipher, &key, &aad_dst, &ct);

    assert!(
        result.is_err(),
        "transplanting ciphertext to different KV key must fail authentication"
    );
}

/// E2E transplant test: verifies `EncryptedKv` wires cell-ID AAD correctly.
///
/// Writes via `put` (which sets AAD = BLAKE3("src-key")), then copies the raw
/// ciphertext to "dst-key" in the inner store and tries to read it back.
/// The read must fail because the AAD for "dst-key" differs.
#[test]
fn transplant_attack_fails_through_encrypted_kv() {
    let enc = enc_xchacha();
    enc.put(b"src-key", b"value").expect("put");

    // Copy the raw ciphertext to "dst-key" in the unencrypted inner store.
    let raw_ct = enc
        .inner_ref()
        .get(b"src-key")
        .expect("inner get")
        .expect("exists");
    enc.inner_ref()
        .put(b"dst-key", &raw_ct)
        .expect("inner put dst");

    // Reading as "dst-key" must fail — AAD is BLAKE3("dst-key") ≠ BLAKE3("src-key").
    let result = enc.get(b"dst-key");
    assert!(
        result.is_err(),
        "transplanting raw ciphertext to a different key must fail via EncryptedKv"
    );
}

// ── EncryptedTxn tests ────────────────────────────────────────────────────────

#[test]
fn encrypted_txn_put_commit_get() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        XChaCha20Poly1305Aead,
    );

    // Write some initial data.
    enc.put(b"pre-existing", b"before").expect("initial put");

    // Open a transaction, write new data, and commit.
    {
        let mut txn = enc.transaction().expect("txn failed");
        txn.put(b"txn-key", b"txn-value").expect("txn put failed");
        txn.commit().expect("commit failed");
    }

    // The committed value must be readable.
    let result = enc.get(b"txn-key").expect("get after commit");
    assert_eq!(result, Some(b"txn-value".to_vec()));

    // Pre-existing data must still be readable.
    let result2 = enc.get(b"pre-existing").expect("pre-existing get");
    assert_eq!(result2, Some(b"before".to_vec()));
}

#[test]
fn encrypted_txn_rollback_discards_changes() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        XChaCha20Poly1305Aead,
    );

    enc.put(b"stable", b"original").expect("initial put");

    // Open a transaction, write, then rollback.
    {
        let mut txn = enc.transaction().expect("txn failed");
        txn.put(b"ephemeral", b"should-vanish").expect("txn put");
        txn.rollback().expect("rollback failed");
    }

    // The rolled-back key must not exist.
    let result = enc.get(b"ephemeral").expect("get after rollback");
    assert_eq!(result, None, "rolled-back key must not be visible");

    // The pre-existing key must be unchanged.
    let result2 = enc.get(b"stable").expect("stable get");
    assert_eq!(result2, Some(b"original".to_vec()));
}

#[test]
fn encrypted_txn_get_reads_own_writes() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        XChaCha20Poly1305Aead,
    );

    let mut txn = enc.transaction().expect("txn");
    txn.put(b"k", b"v").expect("put");
    let got = txn.get(b"k").expect("get");
    assert_eq!(got, Some(b"v".to_vec()), "txn must see its own writes");
    txn.commit().expect("commit");
}

#[test]
fn encrypted_txn_delete_removes_within_txn() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        XChaCha20Poly1305Aead,
    );
    enc.put(b"delete-me", b"value").expect("put");

    let mut txn = enc.transaction().expect("txn");
    txn.delete(b"delete-me").expect("delete in txn");
    txn.commit().expect("commit");

    let result = enc.get(b"delete-me").expect("get after txn delete");
    assert_eq!(result, None);
}

#[test]
fn encrypted_txn_aes_gcm_siv_round_trip() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        AesGcmSiv256Aead,
    );

    let mut txn = enc.transaction().expect("txn");
    txn.put(b"aes-key", b"aes-value").expect("txn put");
    txn.commit().expect("commit");

    let result = enc.get(b"aes-key").expect("get");
    assert_eq!(result, Some(b"aes-value".to_vec()));
}

// ── EncryptedSnapshot tests ───────────────────────────────────────────────────

#[test]
fn encrypted_snapshot_reads_state_at_capture_time() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        XChaCha20Poly1305Aead,
    );

    enc.put(b"snap-key", b"original").expect("put");

    // Capture snapshot before overwriting.
    let snap = enc.snapshot().expect("snapshot");

    // Overwrite the key.
    enc.put(b"snap-key", b"updated").expect("overwrite");

    // Snapshot must reflect the pre-overwrite state.
    let snap_val = snap.get(b"snap-key").expect("snap get");
    assert_eq!(
        snap_val,
        Some(b"original".to_vec()),
        "snapshot must reflect state at capture time"
    );

    // Live store must see the update.
    let live_val = enc.get(b"snap-key").expect("live get");
    assert_eq!(live_val, Some(b"updated".to_vec()));
}

#[test]
fn encrypted_snapshot_absent_key_returns_none() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        XChaCha20Poly1305Aead,
    );
    let snap = enc.snapshot().expect("snapshot");
    let result = snap.get(b"no-such-key").expect("snap get");
    assert_eq!(result, None);
}

#[test]
fn encrypted_snapshot_later_writes_not_visible() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        XChaCha20Poly1305Aead,
    );
    let snap = enc.snapshot().expect("snapshot");

    // Write after snapshot is taken.
    enc.put(b"new-key", b"new-value").expect("put after snap");

    // Snapshot must not see the new key.
    let result = snap.get(b"new-key").expect("snap get new key");
    assert_eq!(result, None, "snapshot must not see post-capture writes");
}

#[test]
fn encrypted_snapshot_aes_gcm_siv_round_trip() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        AesGcmSiv256Aead,
    );

    enc.put(b"a", b"alpha").expect("put");
    let snap = enc.snapshot().expect("snapshot");
    let result = snap.get(b"a").expect("snap get");
    assert_eq!(result, Some(b"alpha".to_vec()));
}

// ── CipherBuilder tests ───────────────────────────────────────────────────────

#[test]
fn cipher_builder_xchacha20_raw_key() {
    let enc = CipherBuilder::new()
        .aead(AeadChoice::XChaCha20Poly1305)
        .key(key_a())
        .build(MemStore::default())
        .expect("build failed");

    enc.put(b"x", b"chacha").expect("put");
    let result = enc.get(b"x").expect("get");
    assert_eq!(result, Some(b"chacha".to_vec()));
}

#[test]
fn cipher_builder_aes_gcm_siv_raw_key() {
    let enc = CipherBuilder::new()
        .aead(AeadChoice::AesGcmSiv256)
        .key(key_a())
        .build(MemStore::default())
        .expect("build failed");

    enc.put(b"x", b"aes-siv").expect("put");
    let result = enc.get(b"x").expect("get");
    assert_eq!(result, Some(b"aes-siv".to_vec()));
}

#[test]
fn cipher_builder_passphrase_key_xchacha20() {
    let salt = [0x55u8; 32];
    let enc = CipherBuilder::new()
        .aead(AeadChoice::XChaCha20Poly1305)
        .passphrase(b"test-passphrase".to_vec(), salt)
        .build_test(MemStore::default())
        .expect("build failed");

    enc.put(b"pass-key", b"pass-value").expect("put");
    let result = enc.get(b"pass-key").expect("get");
    assert_eq!(result, Some(b"pass-value".to_vec()));
}

#[test]
fn cipher_builder_passphrase_key_aes_gcm_siv() {
    let salt = [0x77u8; 32];
    let enc = CipherBuilder::new()
        .aead(AeadChoice::AesGcmSiv256)
        .passphrase(b"my-passphrase".to_vec(), salt)
        .build_test(MemStore::default())
        .expect("build failed");

    enc.put(b"aes-pp-key", b"aes-pp-val").expect("put");
    let result = enc.get(b"aes-pp-key").expect("get");
    assert_eq!(result, Some(b"aes-pp-val".to_vec()));
}

#[test]
fn cipher_builder_missing_key_returns_error() {
    let result = CipherBuilder::new()
        .aead(AeadChoice::XChaCha20Poly1305)
        .build(MemStore::default());
    assert!(result.is_err(), "missing key must produce an error");
}

// ── AeadKind enum dispatch ────────────────────────────────────────────────────

#[test]
fn aead_kind_xchacha20_round_trip() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        AeadKind::XChaCha20Poly1305,
    );
    enc.put(b"k1", b"v1").expect("put");
    assert_eq!(enc.get(b"k1").expect("get"), Some(b"v1".to_vec()));
}

#[test]
fn aead_kind_aes_gcm_siv_round_trip() {
    let enc = EncryptedKv::with_aead(
        MemStore::default(),
        StaticKey::from_array(key_a()),
        AeadKind::AesGcmSiv256,
    );
    enc.put(b"k2", b"v2").expect("put");
    assert_eq!(enc.get(b"k2").expect("get"), Some(b"v2".to_vec()));
}

// ── Existing EncryptedKv::new still works (backward compat) ──────────────────

#[test]
fn encrypted_kv_new_backward_compat() {
    let enc = EncryptedKv::new(MemStore::default(), StaticKey::from_array(key_a()));
    enc.put(b"compat", b"yes").expect("put");
    assert_eq!(enc.get(b"compat").expect("get"), Some(b"yes".to_vec()));
}

// ── iter and range decrypt with new AEAD ─────────────────────────────────────

#[test]
fn aes_gcm_siv_iter_decrypts_all_values() {
    let enc = enc_aes_gcm_siv();
    enc.put(b"aaa", b"111").expect("put a");
    enc.put(b"bbb", b"222").expect("put b");
    enc.put(b"ccc", b"333").expect("put c");

    let mut items: Vec<(Vec<u8>, Vec<u8>)> = enc
        .iter()
        .expect("iter")
        .map(|r| r.expect("item"))
        .collect();
    items.sort_by_key(|(k, _)| k.clone());

    assert_eq!(items.len(), 3);
    assert_eq!(items[0], (b"aaa".to_vec(), b"111".to_vec()));
    assert_eq!(items[1], (b"bbb".to_vec(), b"222".to_vec()));
    assert_eq!(items[2], (b"ccc".to_vec(), b"333".to_vec()));
}

#[test]
fn xchacha20_range_decrypts_values() {
    let enc = enc_xchacha();
    enc.put(b"m-a", b"va").expect("put a");
    enc.put(b"m-b", b"vb").expect("put b");
    enc.put(b"m-c", b"vc").expect("put c");

    let items: Vec<_> = enc
        .range(b"m-a", b"m-c")
        .expect("range")
        .map(|r| r.expect("item"))
        .collect();

    // Range [m-a, m-c) should include m-a and m-b only.
    assert_eq!(items.len(), 2);
}

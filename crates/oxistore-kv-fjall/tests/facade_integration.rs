//! Integration test: `FjallStore` exercised through the `KvStore` trait surface.
//!
//! The `oxistore` facade dispatches `open_with(StoreKind::Fjall, path)` to
//! `FjallStore::open(path)` and boxes the result as `Box<dyn KvStore>`.  This
//! file reproduces that same dispatch manually, verifying that every operation
//! called through the trait abstraction (i.e. via dynamic dispatch) works
//! correctly.
//!
//! Note: adding `oxistore` directly as a dev-dependency here would create a
//! circular dependency (`oxistore` → `oxistore-kv-fjall` → `oxistore`).  The
//! equivalent integration test exercising the facade crate's `open_with` entry
//! point lives in `oxistore/tests/smoke.rs` and `oxistore/tests/cross_backend.rs`,
//! both gated behind `#[cfg(feature = "kv-fjall")]`.

#![forbid(unsafe_code)]

use oxistore_core::{BoxKvStore, StoreError};
use oxistore_kv_fjall::FjallStore;

// ── Helper ────────────────────────────────────────────────────────────────────

fn open_fjall(label: &str) -> BoxKvStore {
    let dir = std::env::temp_dir().join(format!(
        "oxistore_facade_integ_{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    // Mimic exactly what `oxistore::open_with(StoreKind::Fjall, path)` does:
    //   FjallStore::open(path).map(|s| Box::new(s) as BoxKvStore)
    Box::new(FjallStore::open(&dir).expect("FjallStore::open failed"))
}

// ── Basic CRUD ────────────────────────────────────────────────────────────────

#[test]
fn facade_fjall_basic_crud() {
    let store: BoxKvStore = open_fjall("crud");

    // put → get round-trip
    store.put(b"hello", b"world").expect("put");
    assert_eq!(
        store.get(b"hello").expect("get").as_deref(),
        Some(b"world".as_ref()),
        "get must return inserted value"
    );

    // overwrite
    store.put(b"hello", b"WORLD").expect("overwrite");
    assert_eq!(
        store.get(b"hello").expect("get after overwrite").as_deref(),
        Some(b"WORLD".as_ref()),
        "second put must overwrite"
    );

    // contains
    assert!(
        store.contains(b"hello").expect("contains"),
        "contains must be true after put"
    );

    // delete
    store.delete(b"hello").expect("delete");
    assert!(
        store.get(b"hello").expect("get after delete").is_none(),
        "get must return None after delete"
    );
    assert!(
        !store.contains(b"hello").expect("contains after delete"),
        "contains must be false after delete"
    );
}

// ── Range scan ────────────────────────────────────────────────────────────────

#[test]
fn facade_fjall_range_scan() {
    let store: BoxKvStore = open_fjall("range");

    for i in 0u8..10 {
        store.put(&[i], &[i * 10]).expect("put");
    }

    let pairs: Vec<_> = store
        .range(&[2u8], &[7u8])
        .expect("range")
        .map(|r| r.expect("item"))
        .collect();

    assert_eq!(pairs.len(), 5, "range [2,7) should return 5 items");
    for (k, _) in &pairs {
        assert!(k[0] >= 2 && k[0] < 7, "unexpected key in range: {k:?}");
    }
}

// ── Prefix scan ───────────────────────────────────────────────────────────────

#[test]
fn facade_fjall_prefix_scan() {
    let store: BoxKvStore = open_fjall("prefix");

    for i in 0..4 {
        store
            .put(format!("user:{i}").as_bytes(), b"data")
            .expect("put user");
        store
            .put(format!("order:{i}").as_bytes(), b"data")
            .expect("put order");
    }

    let users: Vec<_> = store
        .prefix_scan(b"user:")
        .expect("prefix_scan")
        .map(|r| r.expect("item"))
        .collect();

    assert_eq!(users.len(), 4, "prefix_scan('user:') must return 4 items");
    for (k, _) in &users {
        assert!(
            k.starts_with(b"user:"),
            "unexpected key returned by prefix_scan: {k:?}"
        );
    }
}

// ── Transaction (commit and rollback) ─────────────────────────────────────────

#[test]
fn facade_fjall_transaction_commit() {
    let store: BoxKvStore = open_fjall("txn_commit");

    let mut txn = store.transaction().expect("transaction");
    txn.put(b"txn_key", b"txn_val").expect("txn put");
    txn.commit().expect("commit");

    assert_eq!(
        store.get(b"txn_key").expect("get after commit").as_deref(),
        Some(b"txn_val".as_ref()),
        "committed value must be visible"
    );
}

#[test]
fn facade_fjall_transaction_rollback() {
    let store: BoxKvStore = open_fjall("txn_rollback");

    let mut txn = store.transaction().expect("transaction");
    txn.put(b"rollback_key", b"should_vanish").expect("txn put");
    txn.rollback().expect("rollback");

    assert!(
        store
            .get(b"rollback_key")
            .expect("get after rollback")
            .is_none(),
        "rolled-back key must not appear in the store"
    );
}

// ── Snapshot isolation ────────────────────────────────────────────────────────

#[test]
fn facade_fjall_snapshot_isolation() {
    let store: BoxKvStore = open_fjall("snap");

    store.put(b"snap_k", b"v1").expect("put v1");
    let snap = store.snapshot().expect("snapshot");

    store.put(b"snap_k", b"v2").expect("overwrite");

    // Snapshot must still see v1.
    assert_eq!(
        snap.get(b"snap_k").expect("snap get").as_deref(),
        Some(b"v1".as_ref()),
        "snapshot must be point-in-time isolated"
    );
    // Live store sees v2.
    assert_eq!(
        store.get(b"snap_k").expect("live get").as_deref(),
        Some(b"v2".as_ref()),
        "live store must see the overwritten value"
    );
}

// ── Flush ─────────────────────────────────────────────────────────────────────

#[test]
fn facade_fjall_flush_no_error() {
    let store: BoxKvStore = open_fjall("flush");
    store.put(b"flush_k", b"flush_v").expect("put");
    store.flush().expect("flush must not error");
}

// ── Batch operations ──────────────────────────────────────────────────────────

#[test]
fn facade_fjall_batch_write_and_delete() {
    let store: BoxKvStore = open_fjall("batch");

    let pairs: Vec<(&[u8], &[u8])> = vec![
        (b"b0", b"0"),
        (b"b1", b"1"),
        (b"b2", b"2"),
        (b"b3", b"3"),
        (b"b4", b"4"),
    ];
    store.batch_write(&pairs).expect("batch_write");

    for &(k, v) in &pairs {
        assert_eq!(
            store.get(k).expect("get after batch_write").as_deref(),
            Some(v),
            "batch_write must have written all pairs"
        );
    }

    let del_keys: Vec<&[u8]> = vec![b"b1", b"b3"];
    store.batch_delete(&del_keys).expect("batch_delete");

    assert!(store.get(b"b1").expect("get b1").is_none(), "b1 deleted");
    assert!(store.get(b"b3").expect("get b3").is_none(), "b3 deleted");
    assert!(store.get(b"b0").expect("get b0").is_some(), "b0 intact");
    assert!(store.get(b"b2").expect("get b2").is_some(), "b2 intact");
    assert!(store.get(b"b4").expect("get b4").is_some(), "b4 intact");
}

// ── Error-type compatibility with StoreError ──────────────────────────────────

#[test]
fn facade_fjall_store_error_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<StoreError>();
}

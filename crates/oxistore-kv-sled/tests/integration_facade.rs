//! Integration tests: sled backend via the `oxistore` facade.
//!
//! These tests exercise `oxistore::open_with(StoreKind::Sled, path)` and
//! `oxistore::open_in_memory(StoreKind::Sled)` to confirm that the sled
//! backend is correctly wired into the facade.

use oxistore::{open_in_memory, open_with, StoreKind};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_path(label: &str) -> std::path::PathBuf {
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "oxistore-sled-facade-{label}-{}-{seq}",
        std::process::id()
    ))
}

// ── open_in_memory via facade ──────────────────────────────────────────────────

#[test]
fn facade_open_in_memory_sled_put_get() {
    let store = open_in_memory(StoreKind::Sled).expect("open_in_memory sled");
    store.put(b"hello", b"world").expect("put");
    let val = store.get(b"hello").expect("get");
    assert_eq!(val.as_deref(), Some(b"world".as_ref()));
}

#[test]
fn facade_open_in_memory_sled_delete() {
    let store = open_in_memory(StoreKind::Sled).expect("open_in_memory sled");
    store.put(b"key", b"value").expect("put");
    store.delete(b"key").expect("delete");
    assert!(store.get(b"key").expect("get after delete").is_none());
}

#[test]
fn facade_open_in_memory_sled_range_scan() {
    let store = open_in_memory(StoreKind::Sled).expect("open_in_memory sled");
    for i in 0u8..10 {
        store.put(&[i], &[i * 2]).expect("put");
    }

    let results: Vec<_> = store
        .range(&[2u8], &[7u8])
        .expect("range")
        .collect::<Result<_, _>>()
        .expect("collect");

    assert_eq!(results.len(), 5, "range [2,7) should return 5 entries");
    assert_eq!(results[0].0, [2u8]);
    assert_eq!(results[4].0, [6u8]);
}

// ── open_with(StoreKind::Sled, path) ─────────────────────────────────────────

#[test]
fn facade_open_with_sled_path_persistence() {
    let path = unique_path("persist");
    // Drop the store after writing — then reopen and verify.
    {
        let store = open_with(StoreKind::Sled, &path).expect("first open");
        store.put(b"persisted", b"yes").expect("put");
        store.flush().expect("flush");
    }
    let store2 = open_with(StoreKind::Sled, &path).expect("second open");
    let val = store2.get(b"persisted").expect("get after reopen");
    assert_eq!(
        val.as_deref(),
        Some(b"yes".as_ref()),
        "value must persist across reopen"
    );
}

#[test]
fn facade_open_with_sled_transaction_commit() {
    let path = unique_path("txn");
    let store = open_with(StoreKind::Sled, &path).expect("open");

    store.put(b"before", b"old").expect("pre-put");

    let mut txn = store.transaction().expect("begin transaction");
    txn.put(b"new_key", b"new_val").expect("txn put");
    txn.delete(b"before").expect("txn delete");
    txn.commit().expect("commit");

    assert_eq!(
        store.get(b"new_key").expect("get new"),
        Some(b"new_val".to_vec())
    );
    assert!(
        store.get(b"before").expect("get deleted").is_none(),
        "deleted key must be absent after commit"
    );
}

#[test]
fn facade_open_with_sled_snapshot_isolation() {
    let path = unique_path("snap");
    let store = open_with(StoreKind::Sled, &path).expect("open");

    store.put(b"snap_key", b"snap_val").expect("put");
    let snap = store.snapshot().expect("snapshot");

    // Modify after snapshot.
    store.put(b"snap_key", b"modified").expect("overwrite");
    store.put(b"extra", b"invisible").expect("extra put");

    // Snapshot sees original value.
    assert_eq!(
        snap.get(b"snap_key").expect("snap get"),
        Some(b"snap_val".to_vec()),
        "snapshot must see original value"
    );
    // Snapshot does not see new key.
    assert!(
        snap.get(b"extra").expect("snap extra get").is_none(),
        "snapshot must not see post-creation writes"
    );
}

#[test]
fn facade_open_with_sled_prefix_scan() {
    let path = unique_path("pfxscan");
    let store = open_with(StoreKind::Sled, &path).expect("open");

    for i in 0..5u32 {
        store
            .put(format!("user:{i:03}").as_bytes(), b"data")
            .expect("put user");
    }
    for i in 0..5u32 {
        store
            .put(format!("order:{i:03}").as_bytes(), b"data")
            .expect("put order");
    }

    let user_results: Vec<_> = store
        .prefix_scan(b"user:")
        .expect("prefix_scan")
        .collect::<Result<_, _>>()
        .expect("collect");

    assert_eq!(user_results.len(), 5, "must return 5 user entries");
    for (k, _) in &user_results {
        assert!(k.starts_with(b"user:"), "all keys must match prefix");
    }
}

#[test]
fn facade_open_with_sled_batch_write_and_count() {
    let path = unique_path("batch");
    let store = open_with(StoreKind::Sled, &path).expect("open");

    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..100u32)
        .map(|i| {
            (
                format!("batch_key_{i:04}").into_bytes(),
                format!("batch_val_{i}").into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&[u8], &[u8])> = entries
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();

    store.batch_write(&refs).expect("batch_write");
    assert_eq!(store.count().expect("count"), 100);
}

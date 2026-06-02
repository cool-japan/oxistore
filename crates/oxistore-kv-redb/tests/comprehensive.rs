use oxistore_core::KvStore;
use oxistore_kv_redb::{RedbStore, RedbStoreBuilder};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

static COMP_REDB_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxistore_redb_comprehensive_{}_{}_{}_{}",
        name,
        std::process::id(),
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        },
        COMP_REDB_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

// 1. 1K key insert + full range scan
#[test]
fn redb_1k_key_insert_range_scan() {
    let store = RedbStore::open_in_memory().expect("open failed");

    for i in 0u32..1000 {
        let key = format!("key_{:05}", i);
        let val = format!("val_{}", i);
        store.put(key.as_bytes(), val.as_bytes()).expect("put");
    }

    let items: Vec<_> = store
        .iter()
        .expect("iter")
        .collect::<Result<_, _>>()
        .expect("collect");

    assert_eq!(
        items.len(),
        1000,
        "expected 1000 items, got {}",
        items.len()
    );

    // Verify ascending order.
    for w in items.windows(2) {
        assert!(w[0].0 <= w[1].0, "keys must be ascending");
    }
}

// 2. 100K keys (slow, ignored)
#[test]
#[ignore]
fn redb_100k_keys() {
    let store = RedbStore::open_in_memory().expect("open failed");

    for i in 0u32..100_000 {
        let key = format!("key_{:06}", i);
        let val = format!("val_{}", i);
        store.put(key.as_bytes(), val.as_bytes()).expect("put");
    }

    let count = store.count().expect("count");
    assert_eq!(count, 100_000);
}

// 3. Concurrent multi-Arc — 4 reader threads, 25 keys each
#[test]
fn redb_concurrent_multi_arc() {
    let store = RedbStore::open_in_memory().expect("open failed");

    // Pre-write 100 keys.
    for i in 0u32..100 {
        let key = format!("ckey_{:03}", i);
        let val = format!("cval_{}", i);
        store
            .put(key.as_bytes(), val.as_bytes())
            .expect("pre-write");
    }

    let shared = Arc::new(store);

    let handles: Vec<_> = (0u32..4)
        .map(|t| {
            let s = Arc::clone(&shared);
            std::thread::spawn(move || {
                let start = t * 25;
                for i in start..start + 25 {
                    let key = format!("ckey_{:03}", i);
                    let val = s.get(key.as_bytes()).expect("get");
                    assert!(val.is_some(), "key {} must exist", i);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("reader thread panicked");
    }
}

// 4. Snapshot MVCC isolation — snapshot sees pre-write state
#[test]
fn redb_snapshot_sees_pre_write_state() {
    let store = RedbStore::open_in_memory().expect("open failed");
    store.put(b"existing", b"before").expect("put");

    let snap = store.snapshot().expect("snapshot");

    // Write a new key AFTER taking the snapshot.
    store
        .put(b"post_snap_key", b"should_be_invisible")
        .expect("post-snap put");

    assert_eq!(
        snap.get(b"existing").expect("snap get existing"),
        Some(b"before".to_vec())
    );
    assert_eq!(
        snap.get(b"post_snap_key").expect("snap get post"),
        None,
        "snapshot must not see writes made after it was created"
    );
}

// 5. Empty key roundtrip
#[test]
fn redb_empty_key_roundtrip() {
    let store = RedbStore::open_in_memory().expect("open failed");
    store.put(b"", b"empty_key_value").expect("put empty key");
    let val = store.get(b"").expect("get empty key");
    assert_eq!(val.as_deref(), Some(b"empty_key_value".as_ref()));
}

// 6. Large key (1 KB) roundtrip
#[test]
fn redb_large_key_1kb() {
    let store = RedbStore::open_in_memory().expect("open failed");
    let large_key = vec![0xABu8; 1024];
    store
        .put(&large_key, b"large_key_val")
        .expect("put large key");
    let val = store.get(&large_key).expect("get large key");
    assert_eq!(val.as_deref(), Some(b"large_key_val".as_ref()));
}

// 7. Rapid open/close — 10 iterations, same path, verify persistence
#[test]
fn redb_rapid_open_close_10x() {
    let path = unique_path("rapid_open_close");

    for i in 0u32..10 {
        let store = RedbStore::open(&path).expect("open");
        let key = format!("key_{}", i);
        store.put(key.as_bytes(), b"val").expect("put");
        // store drops here, releasing the file lock
    }

    // Reopen and verify the last key (key_9) is present.
    let store = RedbStore::open(&path).expect("reopen");
    assert_eq!(
        store.get(b"key_9").expect("get key_9"),
        Some(b"val".to_vec()),
        "key_9 must survive close/reopen"
    );

    // Cleanup
    let _ = std::fs::remove_file(&path);
}

// 8. TTL lazy expiry via put_with_ttl
#[test]
fn redb_ttl_lazy_expiry() {
    let store = RedbStore::open_in_memory().expect("open failed");
    store
        .put_with_ttl(b"ttl_key", b"ttl_val", Duration::from_millis(50))
        .expect("put_with_ttl");

    // Key should be visible immediately.
    assert_eq!(
        store.get(b"ttl_key").expect("get before expiry"),
        Some(b"ttl_val".to_vec())
    );

    // Wait for TTL to expire.
    std::thread::sleep(Duration::from_millis(200));

    // Lazy eviction triggers on get; key must be gone.
    let val = store.get(b"ttl_key").expect("get after expiry");
    assert_eq!(val, None, "key must be gone after TTL expires");
}

// 9. Transaction read-your-writes (in-txn write then in-txn read)
#[test]
fn redb_transaction_write_then_read() {
    let store = RedbStore::open_in_memory().expect("open failed");
    store.put(b"committed", b"pre").expect("pre-put");

    let mut txn = store.transaction().expect("transaction");
    txn.put(b"txn_key", b"txn_val").expect("txn put");

    // Read-your-writes: the key written inside the txn must be visible.
    let val = txn.get(b"txn_key").expect("txn get");
    assert_eq!(val.as_deref(), Some(b"txn_val".as_ref()));

    // Pre-committed data must also be visible.
    let pre = txn.get(b"committed").expect("txn get committed");
    assert_eq!(pre.as_deref(), Some(b"pre".as_ref()));

    txn.commit().expect("txn commit");

    // Verify committed after txn.
    assert_eq!(
        store.get(b"txn_key").expect("get post-commit"),
        Some(b"txn_val".to_vec())
    );
}

// 10. Batch write 50 pairs, batch delete 25, verify
#[test]
fn redb_batch_write_batch_delete() {
    let store = RedbStore::open_in_memory().expect("open failed");

    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0u32..50)
        .map(|i| {
            (
                format!("bkey_{:02}", i).into_bytes(),
                format!("bval_{}", i).into_bytes(),
            )
        })
        .collect();

    let pairs_ref: Vec<(&[u8], &[u8])> = pairs
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();

    store.batch_write(&pairs_ref).expect("batch_write");

    // All 50 must be present.
    for i in 0u32..50 {
        let key = format!("bkey_{:02}", i);
        let val = store.get(key.as_bytes()).expect("get after batch_write");
        assert!(val.is_some(), "key {} must exist after batch_write", i);
    }

    // Delete keys 0..25.
    let delete_keys: Vec<Vec<u8>> = (0u32..25)
        .map(|i| format!("bkey_{:02}", i).into_bytes())
        .collect();
    let delete_refs: Vec<&[u8]> = delete_keys.iter().map(|k| k.as_slice()).collect();
    store.batch_delete(&delete_refs).expect("batch_delete");

    // Keys 0..25 must be gone.
    for i in 0u32..25 {
        let key = format!("bkey_{:02}", i);
        let val = store.get(key.as_bytes()).expect("get after batch_delete");
        assert_eq!(val, None, "key {} must be absent after batch_delete", i);
    }

    // Keys 25..50 must still be present.
    for i in 25u32..50 {
        let key = format!("bkey_{:02}", i);
        let val = store.get(key.as_bytes()).expect("get surviving key");
        assert!(val.is_some(), "key {} must survive batch_delete", i);
    }
}

// Builder custom cache size
#[test]
fn redb_builder_custom_cache_size() {
    let path = unique_path("builder_cache");
    let store = RedbStoreBuilder::new()
        .cache_size(4 * 1024 * 1024)
        .build(&path)
        .expect("builder build failed");
    store.put(b"hello", b"world").expect("put");
    assert_eq!(store.get(b"hello").expect("get"), Some(b"world".to_vec()));
    let _ = std::fs::remove_file(&path);
}

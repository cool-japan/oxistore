use oxistore_core::KvStore;
use oxistore_kv_sled::SledStore;
use std::sync::Arc;
use std::time::Duration;

fn open_temp() -> SledStore {
    SledStore::open_temporary().expect("open_temporary failed")
}

// 1. Concurrent 8 threads, 100 ops each (alternating put/get with thread-id prefix)
#[test]
fn sled_concurrent_8threads() {
    let store = Arc::new(open_temp());

    // Seed data so reader threads can get.
    for t in 0u32..8 {
        for i in 0u32..100 {
            let key = format!("t{}_{:03}", t, i);
            let val = format!("val_{}", i);
            store.put(key.as_bytes(), val.as_bytes()).expect("seed put");
        }
    }

    let handles: Vec<_> = (0u32..8)
        .map(|t| {
            let s = Arc::clone(&store);
            std::thread::spawn(move || {
                for i in 0u32..100 {
                    let key = format!("t{}_{:03}", t, i);
                    if i % 2 == 0 {
                        let new_val = format!("updated_{}", i);
                        s.put(key.as_bytes(), new_val.as_bytes())
                            .expect("concurrent put");
                    } else {
                        let _val = s.get(key.as_bytes()).expect("concurrent get");
                    }
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }
}

// 2. 50K range scan (slow, ignored)
#[test]
#[ignore]
fn sled_50k_range_scan() {
    let store = open_temp();

    for i in 0u32..50_000 {
        let key = format!("rkey_{:06}", i);
        let val = format!("rval_{}", i);
        store.put(key.as_bytes(), val.as_bytes()).expect("put");
    }

    let count = store.count().expect("count");
    assert_eq!(count, 50_000);
}

// 3. TTL expiry
#[test]
fn sled_ttl_expiry() {
    let store = open_temp();
    store
        .put_with_ttl(b"ttl_key", b"ttl_val", Duration::from_millis(50))
        .expect("put_with_ttl");

    // Key must be visible immediately.
    assert_eq!(
        store.get(b"ttl_key").expect("get before expiry"),
        Some(b"ttl_val".to_vec())
    );

    // Wait for TTL to expire.
    std::thread::sleep(Duration::from_millis(200));

    let val = store.get(b"ttl_key").expect("get after expiry");
    assert_eq!(val, None, "key must be absent after TTL expiry");
}

// 4. Transaction read-your-writes
#[test]
fn sled_transaction_read_your_writes() {
    let store = open_temp();
    store.put(b"existing", b"before").expect("pre-put");

    let mut txn = store.transaction().expect("transaction");
    txn.put(b"txn_write", b"txn_value").expect("txn put");

    // Read-your-writes.
    let val = txn.get(b"txn_write").expect("txn get");
    assert_eq!(val.as_deref(), Some(b"txn_value".as_ref()));

    // Pre-committed key must also be visible.
    let pre = txn.get(b"existing").expect("txn get existing");
    assert_eq!(pre.as_deref(), Some(b"before".as_ref()));

    txn.commit().expect("commit");
    assert_eq!(
        store.get(b"txn_write").expect("post-commit get"),
        Some(b"txn_value".to_vec())
    );
}

// 5. Prefix scan correctness — 10 "pfx_" keys + 10 "other_" keys
#[test]
fn sled_prefix_scan_correctness() {
    let store = open_temp();

    for i in 0u32..10 {
        let key = format!("pfx_{:02}", i);
        store.put(key.as_bytes(), b"pfx_val").expect("put pfx");
    }
    for i in 0u32..10 {
        let key = format!("other_{:02}", i);
        store.put(key.as_bytes(), b"other_val").expect("put other");
    }

    let results: Vec<_> = store
        .prefix_scan(b"pfx_")
        .expect("prefix_scan")
        .collect::<Result<_, _>>()
        .expect("collect");

    assert_eq!(
        results.len(),
        10,
        "prefix_scan must return exactly 10 keys with prefix 'pfx_'"
    );

    for (key, _val) in &results {
        assert!(
            key.starts_with(b"pfx_"),
            "all returned keys must have prefix 'pfx_'"
        );
    }
}

// 6. Compare-and-swap success
#[test]
fn sled_compare_and_swap_success() {
    let store = open_temp();
    store.put(b"cas_key", b"initial").expect("put");

    let ok = store
        .compare_and_swap(b"cas_key", Some(b"initial"), b"updated")
        .expect("cas");
    assert!(ok, "CAS must succeed when expected value matches");

    let val = store.get(b"cas_key").expect("get after cas");
    assert_eq!(val.as_deref(), Some(b"updated".as_ref()));
}

// 7. Compare-and-swap failure with wrong expected value
#[test]
fn sled_compare_and_swap_failure() {
    let store = open_temp();
    store.put(b"cas_key2", b"current").expect("put");

    let ok = store
        .compare_and_swap(b"cas_key2", Some(b"wrong_expected"), b"new_value")
        .expect("cas");
    assert!(!ok, "CAS must fail when expected value does not match");

    // Value must be unchanged.
    let val = store.get(b"cas_key2").expect("get after failed cas");
    assert_eq!(val.as_deref(), Some(b"current".as_ref()));
}

// 8. Delete a key that was never inserted — must not error
#[test]
fn sled_delete_nonexistent_is_ok() {
    let store = open_temp();
    let result = store.delete(b"never_inserted_key");
    assert!(result.is_ok(), "delete of absent key must not error");
}

// 9. Count accuracy — insert 7, delete 2, verify count
#[test]
fn sled_count_accuracy() {
    let store = open_temp();

    for i in 0u32..7 {
        let key = format!("cnt_{}", i);
        store.put(key.as_bytes(), b"v").expect("put");
    }
    assert_eq!(store.count().expect("count after 7 inserts"), 7);

    store.delete(b"cnt_0").expect("delete 0");
    store.delete(b"cnt_3").expect("delete 3");
    assert_eq!(store.count().expect("count after 2 deletes"), 5);
}

// 10. Keys listing — insert 5 unique keys, verify keys() returns all 5
#[test]
fn sled_keys_listing() {
    let store = open_temp();

    let expected_keys: Vec<Vec<u8>> = (0u32..5)
        .map(|i| format!("listkey_{}", i).into_bytes())
        .collect();

    for key in &expected_keys {
        store.put(key, b"val").expect("put");
    }

    let mut returned_keys: Vec<Vec<u8>> = store
        .keys()
        .expect("keys")
        .collect::<Result<_, _>>()
        .expect("collect");

    returned_keys.sort();

    assert_eq!(
        returned_keys.len(),
        5,
        "keys() must return all 5 inserted keys"
    );

    let mut sorted_expected = expected_keys.clone();
    sorted_expected.sort();

    assert_eq!(
        returned_keys, sorted_expected,
        "keys() must return all inserted keys"
    );
}

// Snapshot isolation
#[test]
fn sled_snapshot_isolation() {
    let store = open_temp();
    store.put(b"before_snap", b"visible").expect("put");

    let snap = store.snapshot().expect("snapshot");
    store
        .put(b"after_snap", b"invisible")
        .expect("post-snap put");

    assert_eq!(
        snap.get(b"before_snap").expect("snap get before"),
        Some(b"visible".to_vec())
    );
    assert_eq!(
        snap.get(b"after_snap").expect("snap get after"),
        None,
        "snapshot must not see post-creation writes"
    );
}

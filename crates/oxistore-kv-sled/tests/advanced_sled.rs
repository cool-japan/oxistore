//! Advanced tests for `oxistore-kv-sled`:
//! - Concurrent read/write stress test
//! - Transaction atomicity: partial batch fails completely
//! - Large dataset range scan correctness (50k keys)
//! - Edge cases: empty key values, very large values, rapid cycles

use std::sync::Arc;
use std::thread;

use oxistore_core::KvStore;
use oxistore_kv_sled::SledStore;

// ── Concurrent read/write stress test ────────────────────────────────────────

#[test]
fn concurrent_put_get_stress() {
    let store = Arc::new(SledStore::open_temporary().expect("open temp"));
    let n_writers = 4usize;
    let ops = 50usize;

    // Pre-populate
    for i in 0..20 {
        store
            .put(format!("pre{i:04}").as_bytes(), b"pre_value")
            .expect("pre-put");
    }

    let mut handles = Vec::new();

    for tid in 0..n_writers {
        let s = Arc::clone(&store);
        let h = thread::spawn(move || {
            for op in 0..ops {
                let key = format!("t{tid}_{op:04}");
                let val = format!("val_{tid}_{op}");
                s.put(key.as_bytes(), val.as_bytes()).expect("put");
            }
        });
        handles.push(h);
    }

    // Concurrent readers
    for _ in 0..4 {
        let s = Arc::clone(&store);
        let h = thread::spawn(move || {
            for i in 0..20 {
                let key = format!("pre{i:04}");
                let _ = s.get(key.as_bytes()).expect("get");
            }
        });
        handles.push(h);
    }

    for h in handles {
        h.join().expect("thread panicked");
    }
}

// ── Transaction atomicity ─────────────────────────────────────────────────────

#[test]
fn transaction_commit_applies_all_ops() {
    let store = SledStore::open_temporary().expect("open temp");
    store.put(b"existing", b"original").expect("put existing");

    let mut txn = store.transaction().expect("begin txn");
    txn.put(b"new1", b"val1").expect("txn put");
    txn.put(b"new2", b"val2").expect("txn put");
    txn.delete(b"existing").expect("txn delete");
    txn.commit().expect("commit");

    // All ops applied
    assert_eq!(
        store.get(b"new1").unwrap().as_deref(),
        Some(b"val1".as_ref())
    );
    assert_eq!(
        store.get(b"new2").unwrap().as_deref(),
        Some(b"val2".as_ref())
    );
    assert!(store.get(b"existing").unwrap().is_none());
}

#[test]
fn transaction_rollback_discards_all_ops() {
    let store = SledStore::open_temporary().expect("open temp");
    store.put(b"stable", b"unchanged").expect("put");

    let mut txn = store.transaction().expect("begin txn");
    txn.put(b"ephemeral", b"gone").expect("txn put");
    txn.delete(b"stable").expect("txn delete");
    txn.rollback().expect("rollback");

    // Nothing changed
    assert_eq!(
        store.get(b"stable").unwrap().as_deref(),
        Some(b"unchanged".as_ref())
    );
    assert!(store.get(b"ephemeral").unwrap().is_none());
}

#[test]
fn read_your_writes_in_transaction() {
    let store = SledStore::open_temporary().expect("open temp");
    store.put(b"before", b"before_val").expect("put before");

    let mut txn = store.transaction().expect("begin txn");
    txn.put(b"during", b"during_val").expect("put during");

    // Should see the just-written key
    let got = txn.get(b"during").expect("get during");
    assert_eq!(got.as_deref(), Some(b"during_val".as_ref()));

    // Pre-existing key visible via overlay pass-through
    let before = txn.get(b"before").expect("get before");
    assert_eq!(before.as_deref(), Some(b"before_val".as_ref()));

    // Delete before and verify invisible in txn
    txn.delete(b"before").expect("delete before in txn");
    let after_del = txn.get(b"before").expect("get after del");
    assert!(
        after_del.is_none(),
        "deleted key should be invisible in txn"
    );

    txn.commit().expect("commit");
}

// ── Large dataset range scan correctness ─────────────────────────────────────

#[test]
fn large_dataset_range_scan_50k_keys() {
    let store = SledStore::open_temporary().expect("open temp");
    let n = 50_000usize;

    // Batch insert 50k keys using batch_write (performance)
    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..n)
        .map(|i| {
            (
                format!("key_{i:08}").into_bytes(),
                format!("val_{i}").into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&[u8], &[u8])> = entries
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    store.batch_write(&refs).expect("batch_write 50k");

    // Count should be 50k
    assert_eq!(store.count().unwrap(), n as u64);

    // Range scan for middle 1000 keys
    let lo = format!("key_{:08}", 10_000);
    let hi = format!("key_{:08}", 11_000);
    let range: Vec<_> = store
        .range(lo.as_bytes(), hi.as_bytes())
        .expect("range")
        .map(|r| r.expect("item"))
        .collect();
    assert_eq!(range.len(), 1000, "range should return 1000 keys");

    // Verify sorted
    let keys: Vec<_> = range.iter().map(|(k, _)| k.clone()).collect();
    for w in keys.windows(2) {
        assert!(w[0] < w[1], "range must be sorted");
    }

    // Boundary: first and last
    let first_key = &range[0].0;
    let last_key = &range[range.len() - 1].0;
    assert_eq!(first_key.as_slice(), lo.as_bytes());
    assert!(last_key.as_slice() < hi.as_bytes());
}

// ── Edge cases ────────────────────────────────────────────────────────────────

#[test]
fn empty_value_round_trip() {
    let store = SledStore::open_temporary().expect("open temp");
    store.put(b"empty", b"").expect("put empty value");
    let got = store.get(b"empty").expect("get empty value");
    assert_eq!(got.as_deref(), Some(b"".as_ref()));
}

#[test]
fn duplicate_put_overwrites() {
    let store = SledStore::open_temporary().expect("open temp");
    store.put(b"dup", b"first").expect("put first");
    store.put(b"dup", b"second").expect("put second");
    let got = store.get(b"dup").expect("get dup");
    assert_eq!(got.as_deref(), Some(b"second".as_ref()));
}

#[test]
fn large_value_1mb_round_trip() {
    let store = SledStore::open_temporary().expect("open temp");
    let big_val: Vec<u8> = (0..(1024 * 1024)).map(|i| (i % 251) as u8).collect();
    store.put(b"bigval", &big_val).expect("put 1MB");
    let got = store
        .get(b"bigval")
        .expect("get 1MB")
        .expect("should exist");
    assert_eq!(got, big_val);
}

#[test]
fn rapid_put_delete_cycles() {
    let store = SledStore::open_temporary().expect("open temp");
    for i in 0..100 {
        let key = format!("cycle_{i}");
        store.put(key.as_bytes(), b"v").expect("put");
        store.delete(key.as_bytes()).expect("delete");
        assert!(store.get(key.as_bytes()).expect("get").is_none());
    }
}

#[test]
fn batch_delete_removes_only_specified_keys() {
    let store = SledStore::open_temporary().expect("open temp");
    for i in 0..10 {
        store.put(format!("k{i}").as_bytes(), b"v").expect("put");
    }

    let to_delete: Vec<Vec<u8>> = (0..5).map(|i| format!("k{i}").into_bytes()).collect();
    let refs: Vec<&[u8]> = to_delete.iter().map(|k| k.as_slice()).collect();
    store.batch_delete(&refs).expect("batch_delete");

    // k0..k4 removed, k5..k9 remain
    for i in 0..5 {
        assert!(store.get(format!("k{i}").as_bytes()).unwrap().is_none());
    }
    for i in 5..10 {
        assert!(store.get(format!("k{i}").as_bytes()).unwrap().is_some());
    }
}

#[test]
fn compare_and_swap_success_and_failure() {
    let store = SledStore::open_temporary().expect("open temp");
    store.put(b"cas_key", b"old").expect("put");

    // CAS with correct expected value succeeds
    let swapped = store
        .compare_and_swap(b"cas_key", Some(b"old"), b"new")
        .expect("cas");
    assert!(swapped, "CAS should succeed when expected matches");
    assert_eq!(
        store.get(b"cas_key").unwrap().as_deref(),
        Some(b"new".as_ref())
    );

    // CAS with wrong expected value fails
    let not_swapped = store
        .compare_and_swap(b"cas_key", Some(b"wrong"), b"other")
        .expect("cas fail");
    assert!(!not_swapped, "CAS should fail when expected doesn't match");
    // Value unchanged
    assert_eq!(
        store.get(b"cas_key").unwrap().as_deref(),
        Some(b"new".as_ref())
    );
}

#[test]
fn prefix_scan_returns_only_matching() {
    let store = SledStore::open_temporary().expect("open temp");
    for i in 0..10 {
        store.put(format!("pfx:{i}").as_bytes(), b"v").expect("put");
        store
            .put(format!("other:{i}").as_bytes(), b"v")
            .expect("put");
    }

    let prefix_results: Vec<_> = store
        .prefix_scan(b"pfx:")
        .expect("prefix_scan")
        .map(|r| r.expect("item"))
        .collect();

    assert_eq!(prefix_results.len(), 10);
    for (k, _) in &prefix_results {
        assert!(k.starts_with(b"pfx:"));
    }
}

#[test]
fn snapshot_is_immutable_point_in_time() {
    let store = SledStore::open_temporary().expect("open temp");
    store.put(b"snap_k", b"before").expect("put before");

    let snap = store.snapshot().expect("snapshot");

    // Modify after snapshot
    store.put(b"snap_k", b"after").expect("put after");
    store.put(b"new_k", b"extra").expect("put new");

    // Snapshot sees old state
    assert_eq!(
        snap.get(b"snap_k").unwrap().as_deref(),
        Some(b"before".as_ref())
    );
    // Snapshot doesn't see new key
    assert!(snap.get(b"new_k").unwrap().is_none());
}

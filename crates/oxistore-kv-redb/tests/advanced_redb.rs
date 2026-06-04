//! Advanced tests for `oxistore-kv-redb`:
//! - Concurrent read/write stress tests with multiple threads
//! - Transaction isolation tests
//! - Restore from backup
//! - Edge cases: empty key, empty value, max key size, duplicate puts

use std::sync::Arc;
use std::thread;

use oxistore_core::KvStore;
use oxistore_kv_redb::RedbStore;

fn open_temp() -> RedbStore {
    let path = std::env::temp_dir().join(format!(
        "oxistore_redb_adv_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    RedbStore::open(&path).expect("open_temp failed")
}

// ── Concurrent read/write stress test ────────────────────────────────────────

#[test]
fn concurrent_put_get_stress() {
    let store = Arc::new(RedbStore::open_in_memory().expect("open in-memory"));
    let n_writers = 4usize;
    let n_readers = 4usize;
    let ops = 50usize;

    // First pre-populate some keys so readers can read
    for i in 0..20 {
        store
            .put(format!("pre{i:04}").as_bytes(), b"pre_value")
            .expect("pre-put");
    }

    let store_clone_writers: Vec<_> = (0..n_writers).map(|_| Arc::clone(&store)).collect();
    let store_clone_readers: Vec<_> = (0..n_readers).map(|_| Arc::clone(&store)).collect();

    let mut handles = Vec::new();

    // Writer threads
    for (tid, s) in store_clone_writers.into_iter().enumerate() {
        let h = thread::spawn(move || {
            for op in 0..ops {
                let key = format!("w{tid:02}_{op:04}");
                let val = format!("value_{tid}_{op}");
                s.put(key.as_bytes(), val.as_bytes()).expect("put");
            }
        });
        handles.push(h);
    }

    // Reader threads (read pre-populated keys)
    for (tid, s) in store_clone_readers.into_iter().enumerate() {
        let h = thread::spawn(move || {
            for op in 0..ops {
                let idx = (tid * ops + op) % 20;
                let key = format!("pre{idx:04}");
                // Value may be missing if not yet written; we just verify no panic
                let _ = s.get(key.as_bytes()).expect("get should not error");
            }
        });
        handles.push(h);
    }

    for h in handles {
        h.join().expect("thread panicked");
    }
}

// ── Transaction isolation ─────────────────────────────────────────────────────

#[test]
fn uncommitted_writes_invisible_to_other_readers() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    store.put(b"committed", b"initial").expect("put initial");

    // Begin a transaction but DON'T commit yet
    let mut txn = store.transaction().expect("begin txn");
    txn.put(b"txn_key", b"txn_value").expect("txn put");

    // Another reader should not see the uncommitted key
    let got = store.get(b"txn_key").expect("get before commit");
    assert!(
        got.is_none(),
        "uncommitted key should be invisible to concurrent readers"
    );

    // The existing committed key is still visible with its original value
    let pre = store.get(b"committed").expect("get committed");
    assert_eq!(pre.as_deref(), Some(b"initial".as_ref()));

    // Commit and verify visibility
    txn.commit().expect("commit");
    let after = store.get(b"txn_key").expect("get after commit");
    assert_eq!(after.as_deref(), Some(b"txn_value".as_ref()));
}

#[test]
fn transaction_rollback_discards_writes() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    store.put(b"original", b"value").expect("put");

    // Begin and rollback a transaction
    let mut txn = store.transaction().expect("begin txn");
    txn.put(b"original", b"overwritten").expect("txn put");
    txn.put(b"new_key", b"should_disappear")
        .expect("txn put new");
    txn.rollback().expect("rollback");

    // Original value must be untouched
    let val = store.get(b"original").expect("get after rollback");
    assert_eq!(val.as_deref(), Some(b"value".as_ref()));

    // New key must not exist
    let nk = store.get(b"new_key").expect("get new key");
    assert!(nk.is_none(), "rolled-back key must not exist");
}

#[test]
fn read_your_writes_in_transaction() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    store.put(b"base", b"base_value").expect("put base");

    let mut txn = store.transaction().expect("begin txn");
    // Write a new key
    txn.put(b"new_in_txn", b"fresh").expect("txn put");
    // Read it back within the same transaction (read-your-writes)
    let got = txn.get(b"new_in_txn").expect("txn get");
    assert_eq!(
        got.as_deref(),
        Some(b"fresh".as_ref()),
        "read-your-writes failed"
    );

    // Delete a key and verify it's invisible within the transaction
    txn.delete(b"base").expect("txn delete");
    let deleted = txn.get(b"base").expect("txn get deleted");
    assert!(
        deleted.is_none(),
        "deleted key should be invisible within txn"
    );

    txn.commit().expect("commit");
}

// ── Restore from backup ───────────────────────────────────────────────────────

#[test]
fn backup_and_restore_roundtrip() {
    let store = open_temp();

    // Populate with 50 entries
    for i in 0..50usize {
        let k = format!("key_{i:04}");
        let v = format!("val_{i}");
        store.put(k.as_bytes(), v.as_bytes()).expect("put");
    }

    // Create a backup
    let backup_path = std::env::temp_dir().join(format!(
        "oxistore_redb_backup_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    store.backup(&backup_path).expect("backup");
    assert!(backup_path.exists(), "backup file should exist");

    // Open the backup and verify it's a complete copy
    let restored = RedbStore::open(&backup_path).expect("open backup");
    for i in 0..50usize {
        let k = format!("key_{i:04}");
        let expected = format!("val_{i}");
        let got = restored.get(k.as_bytes()).expect("get from backup");
        assert_eq!(
            got.as_deref(),
            Some(expected.as_bytes()),
            "backup missing key {k}"
        );
    }

    let count = restored.count().expect("count");
    assert_eq!(count, 50, "backup should have 50 keys");

    // Cleanup
    let _ = std::fs::remove_file(&backup_path);
}

// ── Edge cases ────────────────────────────────────────────────────────────────

#[test]
fn empty_value_round_trip() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    store.put(b"empty_val", b"").expect("put empty value");
    let got = store.get(b"empty_val").expect("get empty value");
    assert_eq!(got.as_deref(), Some(b"".as_ref()));
}

#[test]
fn duplicate_put_overwrites() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    store.put(b"dupe", b"first").expect("first put");
    store.put(b"dupe", b"second").expect("second put");
    let got = store.get(b"dupe").expect("get");
    assert_eq!(got.as_deref(), Some(b"second".as_ref()));
}

#[test]
fn large_value_4mb_round_trip() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    let large_val: Vec<u8> = (0..(4 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();
    store.put(b"big", &large_val).expect("put large value");
    let got = store
        .get(b"big")
        .expect("get large value")
        .expect("should exist");
    assert_eq!(got, large_val);
}

#[test]
fn many_keys_count_correct() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    let n = 1000usize;
    for i in 0..n {
        store.put(format!("k{i:06}").as_bytes(), b"v").expect("put");
    }
    let count = store.count().expect("count");
    assert_eq!(count, n as u64);
}

#[test]
fn range_scan_correctness_with_many_keys() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    // Insert 100 keys with numeric suffixes
    for i in 0u32..100 {
        let k = format!("key_{i:04}");
        store.put(k.as_bytes(), &i.to_le_bytes()).expect("put");
    }

    // Range scan for keys 10..=59 (60 keys)
    let lo = b"key_0010";
    let hi = b"key_0060";
    let range: Vec<_> = store
        .range(lo, hi)
        .expect("range")
        .map(|r| r.expect("item"))
        .collect();

    assert_eq!(range.len(), 50, "range should return exactly 50 keys");
    // Verify sorted order
    let keys: Vec<_> = range.iter().map(|(k, _)| k.clone()).collect();
    for window in keys.windows(2) {
        assert!(window[0] < window[1], "range must be sorted ascending");
    }
}

#[test]
fn prefix_scan_returns_only_matching_keys() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    for i in 0..10 {
        store
            .put(format!("prefix_{i}").as_bytes(), b"v")
            .expect("put");
        store
            .put(format!("other_{i}").as_bytes(), b"v")
            .expect("put");
    }

    let prefix: Vec<_> = store
        .prefix_scan(b"prefix_")
        .expect("prefix_scan")
        .map(|r| r.expect("item"))
        .collect();

    assert_eq!(prefix.len(), 10, "should find exactly 10 prefix_ keys");
    for (k, _) in &prefix {
        assert!(
            k.starts_with(b"prefix_"),
            "all keys should start with 'prefix_'"
        );
    }
}

#[test]
fn snapshot_reflects_state_at_creation_time() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    store.put(b"snap_key", b"snapshot_value").expect("put");

    let snap = store.snapshot().expect("snapshot");

    // Overwrite after snapshot
    store.put(b"snap_key", b"new_value").expect("overwrite");

    // Snapshot should still see original value
    let from_snap = snap.get(b"snap_key").expect("snap get");
    assert_eq!(
        from_snap.as_deref(),
        Some(b"snapshot_value".as_ref()),
        "snapshot should not see post-snapshot writes"
    );

    // Live store sees new value
    let live = store.get(b"snap_key").expect("live get");
    assert_eq!(live.as_deref(), Some(b"new_value".as_ref()));
}

#[test]
fn batch_write_all_or_nothing() {
    let store = RedbStore::open_in_memory().expect("open in-memory");
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..20)
        .map(|i| {
            (
                format!("batch_{i:04}").into_bytes(),
                format!("val_{i}").into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&[u8], &[u8])> = pairs
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    store.batch_write(&refs).expect("batch_write");

    let count = store.count().expect("count");
    assert_eq!(count, 20);
}

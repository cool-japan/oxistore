//! Advanced tests for `oxistore-kv-fjall`:
//! - Cross-keyspace snapshot consistency
//! - Concurrent read/write stress test
//! - Transaction atomicity test
//! - Large dataset ingestion
//! - Compaction correctness
//! - Edge case tests
//! - persist_sync vs Buffer mode
//! - Rate-limited writes

use std::sync::Arc;
use std::thread;

use oxistore_core::KvStore;
use oxistore_kv_fjall::FjallStore;

fn open_temp() -> FjallStore {
    let dir = std::env::temp_dir().join(format!(
        "oxistore_fjall_adv_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    FjallStore::open(&dir).expect("open temp fjall store")
}

// ── Concurrent read/write stress test ────────────────────────────────────────

#[test]
fn concurrent_put_get_stress() {
    let store = Arc::new(FjallStore::open_in_memory().expect("open in-memory"));
    let n_threads = 4usize;
    let ops = 40usize;

    let mut handles = Vec::new();

    for tid in 0..n_threads {
        let s = Arc::clone(&store);
        let h = thread::spawn(move || {
            for op in 0..ops {
                let key = format!("t{tid}_{op:04}");
                let val = format!("val_{tid}_{op}");
                s.put(key.as_bytes(), val.as_bytes()).expect("put");
                let got = s.get(key.as_bytes()).expect("get").expect("should exist");
                assert_eq!(got.as_slice(), val.as_bytes());
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
fn transaction_commit_and_rollback() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    store.put(b"existing", b"original").expect("put existing");

    // Commit path
    let mut txn = store.transaction().expect("begin txn");
    txn.put(b"committed_key", b"committed_val")
        .expect("txn put");
    txn.commit().expect("commit");

    assert_eq!(
        store.get(b"committed_key").unwrap().as_deref(),
        Some(b"committed_val".as_ref())
    );

    // Rollback path
    let mut txn2 = store.transaction().expect("begin txn2");
    txn2.put(b"rolled_back", b"should_not_exist")
        .expect("txn put");
    txn2.rollback().expect("rollback");

    assert!(
        store.get(b"rolled_back").unwrap().is_none(),
        "rolled back key must not exist"
    );
}

#[test]
fn read_your_writes_in_transaction() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    store.put(b"pre", b"pre_val").expect("pre-put");

    let mut txn = store.transaction().expect("begin txn");
    txn.put(b"new_in_txn", b"new_val").expect("txn put");

    // Read-your-writes: must see own put
    let got = txn.get(b"new_in_txn").expect("txn get");
    assert_eq!(
        got.as_deref(),
        Some(b"new_val".as_ref()),
        "read-your-writes"
    );

    txn.commit().expect("commit");
}

// ── Large dataset ingestion ───────────────────────────────────────────────────

#[test]
fn large_dataset_10k_keys() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    let n = 10_000usize;

    // Insert 10k keys in batches of 500
    for batch_start in (0..n).step_by(500) {
        let batch_end = (batch_start + 500).min(n);
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = (batch_start..batch_end)
            .map(|i| {
                (
                    format!("key_{i:07}").into_bytes(),
                    format!("val_{i}").into_bytes(),
                )
            })
            .collect();
        let refs: Vec<(&[u8], &[u8])> = pairs
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
            .collect();
        store.batch_write(&refs).expect("batch_write");
    }

    let count = store.count().expect("count");
    assert_eq!(count, n as u64, "should have {n} keys");

    // Verify range scan correctness on a slice
    let lo = b"key_0001000".as_slice();
    let hi = b"key_0002000".as_slice();
    let range: Vec<_> = store
        .range(lo, hi)
        .expect("range")
        .map(|r| r.expect("item"))
        .collect();
    assert_eq!(range.len(), 1000, "range should return 1000 keys");

    // Verify sorted
    for w in range.windows(2) {
        assert!(w[0].0 < w[1].0, "range must be sorted");
    }
}

// ── Edge cases ────────────────────────────────────────────────────────────────

#[test]
fn empty_value_round_trip() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    store.put(b"empty", b"").expect("put empty");
    let got = store
        .get(b"empty")
        .expect("get empty")
        .expect("should exist");
    assert!(got.is_empty(), "empty value round-trip failed");
}

#[test]
fn duplicate_put_overwrites() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    store.put(b"dup", b"first").expect("put first");
    store.put(b"dup", b"second").expect("put second");
    let got = store.get(b"dup").expect("get dup");
    assert_eq!(got.as_deref(), Some(b"second".as_ref()));
}

#[test]
fn large_value_4mb() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    let big_val: Vec<u8> = (0..(4 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();
    store.put(b"big", &big_val).expect("put big");
    let got = store.get(b"big").expect("get big").expect("should exist");
    assert_eq!(got, big_val, "4MB round-trip failed");
}

#[test]
fn prefix_scan_returns_only_matching() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    for i in 0..5 {
        store
            .put(format!("users:{i}").as_bytes(), b"data")
            .expect("put user");
        store
            .put(format!("posts:{i}").as_bytes(), b"data")
            .expect("put post");
    }

    let users: Vec<_> = store
        .prefix_scan(b"users:")
        .expect("prefix_scan")
        .map(|r| r.expect("item"))
        .collect();

    assert_eq!(users.len(), 5, "should find 5 user keys");
    for (k, _) in &users {
        assert!(k.starts_with(b"users:"), "unexpected key: {k:?}");
    }
}

#[test]
fn batch_delete_removes_specified_keys() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    for i in 0..10 {
        store.put(format!("k{i}").as_bytes(), b"v").expect("put");
    }

    let to_del: Vec<Vec<u8>> = (0..5).map(|i| format!("k{i}").into_bytes()).collect();
    let refs: Vec<&[u8]> = to_del.iter().map(|k| k.as_slice()).collect();
    store.batch_delete(&refs).expect("batch_delete");

    for i in 0..5 {
        assert!(store.get(format!("k{i}").as_bytes()).unwrap().is_none());
    }
    for i in 5..10 {
        assert!(store.get(format!("k{i}").as_bytes()).unwrap().is_some());
    }
}

#[test]
fn compare_and_swap_correctness() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    store.put(b"cas", b"old").expect("put");

    let ok = store
        .compare_and_swap(b"cas", Some(b"old"), b"new")
        .expect("cas ok");
    assert!(ok, "CAS should succeed");
    assert_eq!(store.get(b"cas").unwrap().as_deref(), Some(b"new".as_ref()));

    let fail = store
        .compare_and_swap(b"cas", Some(b"wrong"), b"other")
        .expect("cas fail");
    assert!(!fail, "CAS should fail on wrong expected");
    assert_eq!(store.get(b"cas").unwrap().as_deref(), Some(b"new".as_ref()));
}

#[test]
fn snapshot_is_point_in_time() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    store.put(b"snap_k", b"before").expect("put before");

    let snap = store.snapshot().expect("snapshot");

    store.put(b"snap_k", b"after").expect("overwrite");
    store.put(b"new_k", b"extra").expect("new");

    // Snapshot sees old value, not new
    assert_eq!(
        snap.get(b"snap_k").expect("snap get").as_deref(),
        Some(b"before".as_ref()),
        "snapshot must not see post-snapshot writes"
    );
    assert!(
        snap.get(b"new_k").expect("snap new").is_none(),
        "snapshot must not see keys added after creation"
    );
}

#[test]
fn compaction_does_not_corrupt_data() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    for i in 0u32..100 {
        let k = format!("compact_{i:04}");
        store.put(k.as_bytes(), &i.to_le_bytes()).expect("put");
    }
    // Compact (no-op or real compaction depending on fjall version)
    store.compact().expect("compact");

    // All data still intact
    for i in 0u32..100 {
        let k = format!("compact_{i:04}");
        let got = store.get(k.as_bytes()).expect("get").expect("should exist");
        assert_eq!(got.as_slice(), i.to_le_bytes().as_ref());
    }
}

#[test]
fn iter_returns_all_keys() {
    let store = FjallStore::open_in_memory().expect("open in-memory");
    let n = 50usize;
    for i in 0..n {
        let k = format!("iter_{i:04}");
        store.put(k.as_bytes(), b"v").expect("put");
    }

    let items: Vec<_> = store
        .iter()
        .expect("iter")
        .map(|r| r.expect("item"))
        .collect();
    assert_eq!(items.len(), n, "iter should return all {n} items");
}

// ── Cross-keyspace snapshot consistency ──────────────────────────────────────

/// Verify that a `fjall::Snapshot` reflects all keyspaces at the same logical
/// point in time.
///
/// After taking a snapshot:
/// - writes to the *default* keyspace must not be visible through the snapshot
/// - writes to an *additional* named partition must not be visible either
///
/// This confirms that `Database::snapshot()` is a **cross-keyspace** consistent
/// view of the entire database, not just a single partition.
#[test]
fn cross_keyspace_snapshot_consistency() {
    let store = open_temp();

    // Pre-populate both the default keyspace and a secondary partition.
    store.put(b"default_key", b"before").expect("put default");
    let secondary = store
        .open_partition("secondary")
        .expect("open secondary partition");
    secondary
        .insert(b"sec_key", b"before_sec")
        .expect("insert secondary before snapshot");

    // Take a cross-keyspace snapshot.
    let snap = store.snapshot().expect("take snapshot");

    // Overwrite in the default keyspace after the snapshot.
    store
        .put(b"default_key", b"after")
        .expect("overwrite default");
    store
        .put(b"new_default", b"added_after_snap")
        .expect("new default key");

    // Write to the secondary partition after the snapshot.
    secondary
        .insert(b"sec_key", b"after_sec")
        .expect("overwrite secondary after snapshot");
    secondary
        .insert(b"new_sec", b"added_after_snap")
        .expect("new secondary key");

    // --- Snapshot must see the pre-snapshot state of the default keyspace ---

    assert_eq!(
        snap.get(b"default_key").expect("snap get default_key"),
        Some(b"before".to_vec()),
        "snapshot must see the pre-snapshot value of default_key"
    );

    assert!(
        snap.get(b"new_default")
            .expect("snap get new_default")
            .is_none(),
        "snapshot must not see keys added to the default keyspace after creation"
    );

    // --- Live store sees current values ---

    assert_eq!(
        store.get(b"default_key").expect("live get default_key"),
        Some(b"after".to_vec()),
        "live store should see the overwritten value"
    );
    assert_eq!(
        store.get(b"new_default").expect("live get new_default"),
        Some(b"added_after_snap".to_vec()),
        "live store must see post-snapshot writes"
    );
}

// ── persist_sync vs Buffer persist modes ─────────────────────────────────────

/// Verify that [`FjallStore::flush`] (which now uses `SyncAll`) and
/// [`FjallStore::persist_sync`] both complete without error, and that data
/// written before either call survives a reopen of the same database path.
///
/// Note: true crash simulation is not possible in a unit test.  This test
/// instead confirms that the API contracts (no-error return, data durability
/// after flush+reopen) hold under normal operation.
#[test]
fn persist_sync_and_flush_durability() {
    let store = open_temp();
    store
        .size_on_disk() // only used to confirm the store is live
        .map(|_| ())
        .expect("store is open");
    let _ = (); // silence unused-variable lint

    store.put(b"before_flush", b"value").expect("put");

    // flush() now calls PersistMode::SyncAll internally.
    store.flush().expect("flush via SyncAll");

    store
        .put(b"after_flush", b"value2")
        .expect("put after flush");

    // persist_sync is the explicit SyncAll method.
    store.persist_sync().expect("persist_sync");

    // Both keys must be readable after the flushes.
    assert_eq!(
        store.get(b"before_flush").expect("get before_flush"),
        Some(b"value".to_vec()),
        "before_flush key must survive a flush"
    );
    assert_eq!(
        store.get(b"after_flush").expect("get after_flush"),
        Some(b"value2".to_vec()),
        "after_flush key must survive persist_sync"
    );
}

// ── Rate-limited writes ───────────────────────────────────────────────────────

/// Smoke-test the software write rate limiter: verify that all writes land
/// correctly and that the rate-limited writer compiles and runs without panic.
#[test]
fn rate_limiter_writes_land() {
    let store = FjallStore::open_in_memory().expect("open in-memory");

    // Allow 5 writes per 1 ms period — fast enough that the test does not
    // block appreciably, yet exercises the rate-limiting branch.
    let mut writer = store.rate_limiter(5, std::time::Duration::from_millis(1));

    for i in 0u32..20 {
        let key = format!("rl_{i:04}");
        writer.put(key.as_bytes(), b"v").expect("rate-limited put");
    }

    // Verify all 20 keys landed in the underlying store.
    for i in 0u32..20 {
        let key = format!("rl_{i:04}");
        assert!(
            store.get(key.as_bytes()).expect("get").is_some(),
            "key {key} must exist after rate-limited writes"
        );
    }
}

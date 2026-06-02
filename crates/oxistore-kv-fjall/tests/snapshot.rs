use oxistore_core::KvStore;
use oxistore_kv_fjall::FjallStore;
use std::env;
use std::path::PathBuf;

fn temp_path(suffix: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "oxistore_fjall_snap_{}_{}",
        std::process::id(),
        suffix
    ))
}

#[test]
fn snapshot_isolation() {
    let path = temp_path("isolation");
    let store = FjallStore::open(&path).expect("open");

    store.put(b"key", b"before").expect("put before");

    // Open a snapshot — it should see "before" even after a later write.
    let snap = store.snapshot().expect("snapshot");

    store.put(b"key", b"after").expect("put after");

    // The live store should see "after"
    assert_eq!(
        store.get(b"key").expect("get current"),
        Some(b"after".to_vec()),
        "live store should see the latest write"
    );

    // The snapshot should still see "before"
    assert_eq!(
        snap.get(b"key").expect("snap get"),
        Some(b"before".to_vec()),
        "snapshot should reflect state at snapshot creation time"
    );

    drop(snap);
    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn snapshot_range() {
    let path = temp_path("range");
    let store = FjallStore::open(&path).expect("open");

    store.put(&[1], &[1]).expect("put 1");
    store.put(&[2], &[2]).expect("put 2");
    store.put(&[3], &[3]).expect("put 3");

    let snap = store.snapshot().expect("snapshot");

    // Add keys after snapshot — should not be visible in snap range
    store.put(&[4], &[4]).expect("put 4 after snapshot");
    store.put(&[5], &[5]).expect("put 5 after snapshot");

    let snap_keys: Vec<u8> = snap
        .range(&[0], &[0xFF])
        .expect("snap range")
        .map(|r| r.expect("item").0[0])
        .collect();

    assert_eq!(
        snap_keys,
        vec![1, 2, 3],
        "snapshot range should only see keys present at snapshot time"
    );

    drop(snap);
    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn snapshot_read_missing_key() {
    let path = temp_path("missing");
    let store = FjallStore::open(&path).expect("open");
    let snap = store.snapshot().expect("snapshot");

    assert_eq!(
        snap.get(b"absent").expect("snap get absent"),
        None,
        "absent key should return None"
    );

    drop(snap);
    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

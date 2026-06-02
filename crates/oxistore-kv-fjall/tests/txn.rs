use oxistore_core::KvStore;
use oxistore_kv_fjall::FjallStore;
use std::env;
use std::path::PathBuf;

fn temp_path(suffix: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "oxistore_fjall_txn_{}_{}",
        std::process::id(),
        suffix
    ))
}

#[test]
fn transaction_commit() {
    let path = temp_path("commit");
    let store = FjallStore::open(&path).expect("open");

    let mut txn = store.transaction().expect("txn");
    txn.put(b"a", b"1").expect("txn put a");
    txn.put(b"b", b"2").expect("txn put b");
    txn.put(b"c", b"3").expect("txn put c");
    txn.commit().expect("commit");

    assert_eq!(store.get(b"a").expect("get a"), Some(b"1".to_vec()));
    assert_eq!(store.get(b"b").expect("get b"), Some(b"2".to_vec()));
    assert_eq!(store.get(b"c").expect("get c"), Some(b"3".to_vec()));

    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn transaction_rollback() {
    let path = temp_path("rollback");
    let store = FjallStore::open(&path).expect("open");

    store.put(b"existing", b"original").expect("put existing");

    let mut txn = store.transaction().expect("txn");
    txn.put(b"existing", b"modified").expect("txn put");
    txn.put(b"new_key", b"value").expect("txn put new");
    txn.rollback().expect("rollback");

    // After rollback, original state should be preserved
    assert_eq!(
        store.get(b"existing").expect("get existing"),
        Some(b"original".to_vec()),
        "existing key should retain original value after rollback"
    );
    assert_eq!(
        store.get(b"new_key").expect("get new_key"),
        None,
        "new key should not be visible after rollback"
    );

    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn transaction_delete() {
    let path = temp_path("delete");
    let store = FjallStore::open(&path).expect("open");

    store.put(b"to_delete", b"value").expect("put");

    let mut txn = store.transaction().expect("txn");
    txn.delete(b"to_delete").expect("txn delete");
    txn.commit().expect("commit");

    assert_eq!(
        store.get(b"to_delete").expect("get"),
        None,
        "key should be deleted after txn commit"
    );

    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

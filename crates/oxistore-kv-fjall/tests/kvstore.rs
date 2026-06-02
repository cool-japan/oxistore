use oxistore_core::KvStore;
use oxistore_kv_fjall::FjallStore;
use std::env;
use std::path::PathBuf;

fn temp_path(suffix: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "oxistore_fjall_test_{}_{}",
        std::process::id(),
        suffix
    ))
}

#[test]
fn basic_put_get_delete() {
    let path = temp_path("basic");
    let store = FjallStore::open(&path).expect("open");

    store.put(b"hello", b"world").expect("put");
    assert_eq!(
        store.get(b"hello").expect("get"),
        Some(b"world".to_vec()),
        "value should be 'world' after put"
    );

    store.delete(b"hello").expect("delete");
    assert_eq!(
        store.get(b"hello").expect("get after delete"),
        None,
        "value should be None after delete"
    );

    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn overwrite_existing_key() {
    let path = temp_path("overwrite");
    let store = FjallStore::open(&path).expect("open");

    store.put(b"k", b"v1").expect("put v1");
    store.put(b"k", b"v2").expect("put v2");
    assert_eq!(
        store.get(b"k").expect("get"),
        Some(b"v2".to_vec()),
        "value should be updated to v2"
    );

    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn contains_returns_correct_result() {
    let path = temp_path("contains");
    let store = FjallStore::open(&path).expect("open");

    assert!(!store.contains(b"absent").expect("contains absent"));
    store.put(b"present", b"val").expect("put");
    assert!(store.contains(b"present").expect("contains present"));

    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn delete_absent_key_is_noop() {
    let path = temp_path("delete_absent");
    let store = FjallStore::open(&path).expect("open");
    // Should not error
    store.delete(b"nonexistent").expect("delete absent");
    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn range_scan() {
    let path = temp_path("range");
    let store = FjallStore::open(&path).expect("open");

    for i in 0u8..10 {
        store.put(&[i], &[i * 2]).expect("put");
    }

    // [3, 7) — should return keys 3, 4, 5, 6
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = store
        .range(&[3], &[7])
        .expect("range")
        .map(|r| r.expect("item"))
        .collect();

    let keys: Vec<u8> = pairs.iter().map(|(k, _)| k[0]).collect();
    assert_eq!(keys, vec![3, 4, 5, 6], "range [3,7) should return 3,4,5,6");

    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn flush_does_not_error() {
    let path = temp_path("flush");
    let store = FjallStore::open(&path).expect("open");
    store.put(b"key", b"val").expect("put");
    store.flush().expect("flush");
    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

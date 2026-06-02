use oxistore_core::KvStore;
use oxistore_kv_fjall::FjallStore;
use std::env;
use std::path::PathBuf;

fn temp_path(suffix: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "oxistore_fjall_iter_{}_{}",
        std::process::id(),
        suffix
    ))
}

#[test]
fn iteration_is_sorted() {
    let path = temp_path("sorted");
    let store = FjallStore::open(&path).expect("open");

    // Insert 10 keys out of order via range scan (all-key range)
    for i in [5u8, 2, 8, 1, 9, 3, 7, 4, 6, 0] {
        store.put(&[i], &[i * 10]).expect("put");
    }

    // Retrieve via full range scan: [0x00, 0xFF)
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = store
        .range(&[0x00], &[0xFF])
        .expect("range")
        .map(|r| r.expect("item"))
        .collect();

    let keys: Vec<u8> = pairs.iter().map(|(k, _)| k[0]).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "iteration should be in ascending key order");

    // Also verify values are consistent with keys
    for (k, v) in &pairs {
        assert_eq!(v[0], k[0] * 10, "value should be key * 10");
    }

    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn range_excludes_upper_bound() {
    let path = temp_path("exclude_hi");
    let store = FjallStore::open(&path).expect("open");

    for i in 0u8..5 {
        store.put(&[i], &[]).expect("put");
    }

    // [0, 3) should return 0, 1, 2 — NOT 3
    let keys: Vec<u8> = store
        .range(&[0], &[3])
        .expect("range")
        .map(|r| r.expect("item").0[0])
        .collect();

    assert_eq!(keys, vec![0, 1, 2], "upper bound must be exclusive");

    drop(store);
    let _ = std::fs::remove_dir_all(&path);
}

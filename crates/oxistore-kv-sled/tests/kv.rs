use oxistore_core::KvStore;
use oxistore_kv_sled::SledStore;

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn open_temp() -> SledStore {
    use std::sync::atomic::Ordering;
    use std::time::{SystemTime, UNIX_EPOCH};
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "oxistore-sled-test-{}-{}-{}",
        std::process::id(),
        seq,
        nanos,
    ));
    SledStore::open(&dir).expect("open failed")
}

#[test]
fn put_get_roundtrip() {
    let store = open_temp();
    store.put(b"hello", b"world").unwrap();
    let val = store.get(b"hello").unwrap();
    assert_eq!(val.as_deref(), Some(b"world".as_ref()));
}

#[test]
fn get_missing_key_returns_none() {
    let store = open_temp();
    assert_eq!(store.get(b"no-such-key").unwrap(), None);
}

#[test]
fn delete_removes_key() {
    let store = open_temp();
    store.put(b"key", b"val").unwrap();
    store.delete(b"key").unwrap();
    assert_eq!(store.get(b"key").unwrap(), None);
}

#[test]
fn delete_absent_key_is_noop() {
    let store = open_temp();
    store.delete(b"ghost").unwrap(); // must not error
}

#[test]
fn contains_key() {
    let store = open_temp();
    assert!(!store.contains(b"x").unwrap());
    store.put(b"x", b"1").unwrap();
    assert!(store.contains(b"x").unwrap());
}

#[test]
fn range_returns_sorted() {
    let store = open_temp();
    store.put(b"c", b"3").unwrap();
    store.put(b"a", b"1").unwrap();
    store.put(b"b", b"2").unwrap();
    let results: Vec<_> = store
        .range(b"a", b"d")
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].0, b"a");
    assert_eq!(results[1].0, b"b");
    assert_eq!(results[2].0, b"c");
}

#[test]
fn range_exclusive_hi() {
    let store = open_temp();
    store.put(b"a", b"1").unwrap();
    store.put(b"b", b"2").unwrap();
    store.put(b"c", b"3").unwrap();
    let results: Vec<_> = store
        .range(b"a", b"c")
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, b"a");
    assert_eq!(results[1].0, b"b");
}

#[test]
fn transaction_commit() {
    let store = open_temp();
    let mut txn = store.transaction().unwrap();
    txn.put(b"txn-key", b"txn-val").unwrap();
    txn.commit().unwrap();
    assert_eq!(
        store.get(b"txn-key").unwrap().as_deref(),
        Some(b"txn-val".as_ref())
    );
}

#[test]
fn transaction_rollback() {
    let store = open_temp();
    let txn = store.transaction().unwrap();
    txn.rollback().unwrap();
    assert_eq!(store.get(b"nope").unwrap(), None);
}

#[test]
fn transaction_delete_then_commit() {
    let store = open_temp();
    store.put(b"del-me", b"v").unwrap();
    let mut txn = store.transaction().unwrap();
    txn.delete(b"del-me").unwrap();
    txn.commit().unwrap();
    assert_eq!(store.get(b"del-me").unwrap(), None);
}

#[test]
fn snapshot_reads_committed_state() {
    let store = open_temp();
    store.put(b"snap-a", b"1").unwrap();
    store.put(b"snap-b", b"2").unwrap();
    let snap = store.snapshot().unwrap();
    assert_eq!(snap.get(b"snap-a").unwrap().as_deref(), Some(b"1".as_ref()));
    assert_eq!(snap.get(b"snap-b").unwrap().as_deref(), Some(b"2".as_ref()));
}

#[test]
fn snapshot_range() {
    let store = open_temp();
    store.put(b"a", b"1").unwrap();
    store.put(b"b", b"2").unwrap();
    store.put(b"c", b"3").unwrap();
    let snap = store.snapshot().unwrap();
    let results: Vec<_> = snap
        .range(b"a", b"c")
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn flush_is_ok() {
    let store = open_temp();
    store.put(b"f", b"v").unwrap();
    store.flush().unwrap();
}

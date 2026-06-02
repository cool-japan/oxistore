use oxistore_core::KvStore;
use oxistore_kv_redb::RedbStore;
use std::sync::atomic::{AtomicU64, Ordering};

static KV_COUNTER: AtomicU64 = AtomicU64::new(0);

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    nanos ^ KV_COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn open_temp() -> RedbStore {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-redb-test-{}-{}-{}",
        std::process::id(),
        rand_suffix(),
        KV_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    RedbStore::open(&dir).expect("open failed")
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
    // hi = "c" => "c" is excluded
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
    // snapshot sees committed writes
    assert_eq!(snap.get(b"snap-a").unwrap().as_deref(), Some(b"1".as_ref()));
    // writes after snapshot are NOT visible
    store.put(b"snap-c", b"3").unwrap();
    assert_eq!(snap.get(b"snap-c").unwrap(), None);
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
fn in_memory_store_roundtrip() {
    let store = RedbStore::open_in_memory().expect("in-memory open failed");
    store.put(b"mem", b"val").unwrap();
    assert_eq!(store.get(b"mem").unwrap().as_deref(), Some(b"val".as_ref()));
}

#[test]
fn flush_is_ok() {
    let store = open_temp();
    store.put(b"f", b"v").unwrap();
    store.flush().unwrap();
}

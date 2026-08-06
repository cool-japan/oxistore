use oxistore_core::{KvStore, StoreError};
use oxistore_kv_fjall::FjallStore;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static TTL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    nanos ^ TTL_COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn open_temp() -> FjallStore {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-ttl-{}-{}-{}",
        std::process::id(),
        rand_suffix(),
        TTL_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    FjallStore::open(&dir).expect("open failed")
}

/// Basic TTL: key present immediately, absent after expiry.
#[test]
fn ttl_basic_expiry() {
    let store = open_temp();
    // 200ms (rather than a razor-thin 50ms) so the "must be present"
    // assertion immediately below doesn't race the TTL under a loaded,
    // highly-parallel test run.
    store
        .put_with_ttl(b"key1", b"val1", Duration::from_millis(200))
        .expect("put_with_ttl");

    assert_eq!(
        store.get(b"key1").expect("get"),
        Some(b"val1".to_vec()),
        "key must be present before TTL expires"
    );

    std::thread::sleep(Duration::from_millis(500));

    assert_eq!(
        store.get(b"key1").expect("get after expiry"),
        None,
        "key must be absent after TTL"
    );
}

/// Keys without TTL are not affected by time.
#[test]
fn ttl_no_ttl_key_persists() {
    let store = open_temp();
    store.put(b"persistent", b"value").expect("put");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        store.get(b"persistent").expect("get"),
        Some(b"value".to_vec()),
        "non-TTL key must not expire"
    );
}

/// expire() on existing key: key expires after delay.
#[test]
fn ttl_expire_on_existing_key() {
    let store = open_temp();
    store.put(b"key2", b"val2").expect("put");

    // See `ttl_basic_expiry` for why this uses 200ms rather than a
    // razor-thin 50ms.
    store
        .expire(b"key2", Duration::from_millis(200))
        .expect("expire");

    assert_eq!(
        store.get(b"key2").expect("get"),
        Some(b"val2".to_vec()),
        "key must be present before expire elapses"
    );

    std::thread::sleep(Duration::from_millis(500));

    assert_eq!(
        store.get(b"key2").expect("get after expire"),
        None,
        "key must be absent after expire + sleep"
    );
}

/// expire() on non-existent key returns KeyNotFound.
#[test]
fn ttl_expire_missing_key_is_error() {
    let store = open_temp();
    let result = store.expire(b"ghost", Duration::from_millis(100));
    assert!(
        matches!(result, Err(StoreError::KeyNotFound)),
        "expire on missing key must return KeyNotFound, got: {result:?}"
    );
}

/// persist() removes TTL; key survives past original expiry.
#[test]
fn ttl_persist_removes_expiry() {
    let store = open_temp();
    store
        .put_with_ttl(b"key3", b"val3", Duration::from_millis(80))
        .expect("put_with_ttl");

    let removed = store.persist(b"key3").expect("persist");
    assert!(removed, "persist must return true when TTL was present");

    std::thread::sleep(Duration::from_millis(150));

    assert_eq!(
        store.get(b"key3").expect("get after persist"),
        Some(b"val3".to_vec()),
        "persisted key must survive past original TTL"
    );
}

/// persist() on key with no TTL returns false.
#[test]
fn ttl_persist_no_ttl_returns_false() {
    let store = open_temp();
    store.put(b"no_ttl", b"v").expect("put");
    let removed = store.persist(b"no_ttl").expect("persist");
    assert!(!removed, "persist on no-TTL key must return false");
}

/// persist() on non-existent key returns KeyNotFound.
#[test]
fn ttl_persist_missing_key_is_error() {
    let store = open_temp();
    let result = store.persist(b"nonexistent");
    assert!(
        matches!(result, Err(StoreError::KeyNotFound)),
        "persist on missing key must return KeyNotFound"
    );
}

/// ttl() returns remaining duration immediately after put_with_ttl.
#[test]
fn ttl_remaining_duration() {
    let store = open_temp();
    store
        .put_with_ttl(b"key4", b"val4", Duration::from_secs(1))
        .expect("put_with_ttl");

    let remaining = store.ttl(b"key4").expect("ttl").expect("should have TTL");
    assert!(
        remaining >= Duration::from_millis(500),
        "remaining TTL must be at least 500ms, got {remaining:?}"
    );
}

/// ttl() returns None for a key with no TTL.
#[test]
fn ttl_no_ttl_returns_none() {
    let store = open_temp();
    store.put(b"plain", b"v").expect("put");
    let result = store.ttl(b"plain").expect("ttl");
    assert!(result.is_none(), "no-TTL key must return None from ttl()");
}

/// ttl() on non-existent key returns KeyNotFound.
#[test]
fn ttl_missing_key_is_error() {
    let store = open_temp();
    let result = store.ttl(b"ghost");
    assert!(
        matches!(result, Err(StoreError::KeyNotFound)),
        "ttl on missing key must return KeyNotFound"
    );
}

/// purge_expired() deletes only expired keys and returns correct count.
#[test]
fn ttl_purge_expired_count() {
    let store = open_temp();

    store
        .put_with_ttl(b"ex1", b"v1", Duration::from_millis(50))
        .expect("put_with_ttl");
    store
        .put_with_ttl(b"ex2", b"v2", Duration::from_millis(50))
        .expect("put_with_ttl");
    store
        .put_with_ttl(b"ex3", b"v3", Duration::from_millis(50))
        .expect("put_with_ttl");

    store.put(b"keep1", b"k1").expect("put");
    store.put(b"keep2", b"k2").expect("put");

    std::thread::sleep(Duration::from_millis(150));

    let deleted = store.purge_expired().expect("purge_expired");
    assert_eq!(
        deleted, 3,
        "purge_expired must delete exactly 3 expired keys"
    );

    assert_eq!(store.get(b"keep1").expect("get"), Some(b"k1".to_vec()));
    assert_eq!(store.get(b"keep2").expect("get"), Some(b"k2".to_vec()));
    assert_eq!(store.get(b"ex1").expect("get"), None);
    assert_eq!(store.get(b"ex2").expect("get"), None);
    assert_eq!(store.get(b"ex3").expect("get"), None);
}

/// Verify Unsupported error variant formats correctly.
#[test]
fn ttl_unsupported_error_display() {
    let err = StoreError::Unsupported("TTL not supported".to_string());
    assert!(
        format!("{err}").contains("TTL not supported"),
        "Unsupported error must include message"
    );
}

/// An expired key must be invisible to `range`, `prefix_scan`, `iter`, and
/// `count` — not just `get`. Before the fix, `get` honored TTL but these
/// scan/iteration paths returned the stale entry unconditionally (fjall was
/// the one backend that never received the sled/redb TTL-scan fix).
#[test]
fn ttl_expired_key_invisible_to_scans() {
    let store = open_temp();

    store.put(b"pfx:alive", b"a").expect("put");
    store
        .put_with_ttl(b"pfx:dying", b"d", Duration::from_millis(50))
        .expect("put_with_ttl");

    std::thread::sleep(Duration::from_millis(150));

    // range() covering both keys must exclude the expired one.
    let ranged: Vec<Vec<u8>> = store
        .range(b"pfx:", b"pfx;")
        .expect("range")
        .map(|r| r.expect("range item").0)
        .collect();
    assert_eq!(
        ranged,
        vec![b"pfx:alive".to_vec()],
        "range() must not return an expired key"
    );

    // prefix_scan() must also exclude it.
    let scanned: Vec<Vec<u8>> = store
        .prefix_scan(b"pfx:")
        .expect("prefix_scan")
        .map(|r| r.expect("prefix_scan item").0)
        .collect();
    assert_eq!(
        scanned,
        vec![b"pfx:alive".to_vec()],
        "prefix_scan() must not return an expired key"
    );

    // iter() over the whole store must not surface it either.
    let all: Vec<Vec<u8>> = store
        .iter()
        .expect("iter")
        .map(|r| r.expect("iter item").0)
        .collect();
    assert!(
        !all.contains(&b"pfx:dying".to_vec()),
        "iter() must not return an expired key: {all:?}"
    );
    assert!(all.contains(&b"pfx:alive".to_vec()));

    // count() must not count the expired entry.
    assert_eq!(
        store.count().expect("count"),
        1,
        "count() must not include an expired key"
    );

    // keys() must not include it either.
    let ks: Vec<Vec<u8>> = store
        .keys()
        .expect("keys")
        .map(|r| r.expect("keys item"))
        .collect();
    assert!(!ks.contains(&b"pfx:dying".to_vec()));
}

/// A snapshot must exclude keys that had already expired at capture time,
/// keeping it TTL-consistent with `get()`.
#[test]
fn ttl_snapshot_excludes_expired_at_capture() {
    let store = open_temp();

    store.put(b"alive", b"a").expect("put alive");
    store
        .put_with_ttl(b"dead", b"d", Duration::from_millis(50))
        .expect("put_with_ttl");

    // Let the short-TTL key expire before capturing the snapshot.
    std::thread::sleep(Duration::from_millis(150));

    let snap = store.snapshot().expect("snapshot");
    assert_eq!(
        snap.get(b"alive").expect("snap get alive"),
        Some(b"a".to_vec()),
        "non-expired key must be present in snapshot"
    );
    assert_eq!(
        snap.get(b"dead").expect("snap get dead"),
        None,
        "key expired before capture must be absent from snapshot"
    );

    // Range over the snapshot must also exclude the expired key.
    let keys: Vec<Vec<u8>> = snap
        .range(b"", b"\xff")
        .expect("snap range")
        .map(|r| r.expect("item").0)
        .collect();
    assert_eq!(keys, vec![b"alive".to_vec()]);
}

/// A transaction read must honor TTL: a committed-but-expired key is invisible
/// to `get`/`contains`/`range` inside the transaction.
#[test]
fn ttl_txn_get_honors_ttl() {
    let store = open_temp();

    store.put(b"keep", b"k").expect("put keep");
    store
        .put_with_ttl(b"gone", b"g", Duration::from_millis(50))
        .expect("put_with_ttl");

    std::thread::sleep(Duration::from_millis(150));

    let txn = store.transaction().expect("transaction");
    assert_eq!(txn.get(b"keep").expect("txn get keep"), Some(b"k".to_vec()));
    assert_eq!(
        txn.get(b"gone").expect("txn get gone"),
        None,
        "expired committed key must be invisible inside a transaction"
    );
    assert!(!txn.contains(b"gone").expect("txn contains gone"));

    let keys: Vec<Vec<u8>> = txn
        .range(b"", b"\xff")
        .expect("txn range")
        .map(|r| r.expect("item").0)
        .collect();
    assert_eq!(
        keys,
        vec![b"keep".to_vec()],
        "txn range must exclude the expired key"
    );
    txn.rollback().expect("rollback");
}

/// Overwriting a key that previously had a TTL via plain `put` must clear
/// the stale expiry — the fresh value must not be evicted at the old TTL.
#[test]
fn ttl_put_clears_stale_expiry() {
    let store = open_temp();

    store
        .put_with_ttl(b"key5", b"old", Duration::from_millis(80))
        .expect("put_with_ttl");

    // Overwrite with a plain put (no TTL) before the old TTL elapses.
    store.put(b"key5", b"new").expect("put");

    // Wait past the *original* TTL.
    std::thread::sleep(Duration::from_millis(150));

    assert_eq!(
        store.get(b"key5").expect("get"),
        Some(b"new".to_vec()),
        "put() must clear the stale TTL from a prior put_with_ttl call"
    );
}

/// Deleting a key that had a TTL must clear the sidecar TTL entry so `ttl()`
/// cannot report an expiry for an absent key.
#[test]
fn ttl_delete_clears_sidecar_entry() {
    let store = open_temp();

    store
        .put_with_ttl(b"key6", b"v", Duration::from_secs(60))
        .expect("put_with_ttl");
    store.delete(b"key6").expect("delete");

    let result = store.ttl(b"key6");
    assert!(
        matches!(result, Err(StoreError::KeyNotFound)),
        "ttl() on a deleted key must return KeyNotFound, got: {result:?}"
    );
}

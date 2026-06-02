use oxistore_core::{KvStore, StoreError};
use oxistore_kv_sled::SledStore;
use std::time::Duration;

fn open_temp() -> SledStore {
    SledStore::open_temporary().expect("open temporary failed")
}

/// Basic TTL: key present immediately, absent after expiry.
#[test]
fn ttl_basic_expiry() {
    let store = open_temp();
    store
        .put_with_ttl(b"key1", b"val1", Duration::from_millis(50))
        .expect("put_with_ttl");

    // Should be visible immediately.
    assert_eq!(
        store.get(b"key1").expect("get"),
        Some(b"val1".to_vec()),
        "key must be present before TTL expires"
    );

    std::thread::sleep(Duration::from_millis(150));

    // Should be gone after TTL.
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

    store
        .expire(b"key2", Duration::from_millis(50))
        .expect("expire");

    assert_eq!(
        store.get(b"key2").expect("get"),
        Some(b"val2".to_vec()),
        "key must be present before expire elapses"
    );

    std::thread::sleep(Duration::from_millis(150));

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

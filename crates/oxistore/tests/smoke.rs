#![forbid(unsafe_code)]

use oxistore::{open, open_with, StoreKind};

fn temp_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxistore-facade-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ))
}

#[test]
fn open_default_redb_roundtrip() {
    let store = open(temp_dir("redb")).expect("facade open failed");
    store.put(b"k", b"v").expect("put failed");
    assert_eq!(
        store.get(b"k").expect("get failed").as_deref(),
        Some(b"v".as_ref())
    );
}

#[test]
fn open_default_redb_delete() {
    let store = open(temp_dir("redb-del")).expect("facade open failed");
    store.put(b"key", b"val").expect("put failed");
    assert!(store.contains(b"key").expect("contains failed"));
    store.delete(b"key").expect("delete failed");
    assert!(!store.contains(b"key").expect("contains failed"));
}

#[test]
fn open_default_redb_transaction() {
    let store = open(temp_dir("redb-txn")).expect("facade open failed");
    let mut txn = store.transaction().expect("transaction failed");
    txn.put(b"txkey", b"txval").expect("txn put failed");
    txn.commit().expect("commit failed");
    assert_eq!(
        store.get(b"txkey").expect("get failed").as_deref(),
        Some(b"txval".as_ref())
    );
}

#[test]
fn open_default_redb_snapshot() {
    let store = open(temp_dir("redb-snap")).expect("facade open failed");
    store.put(b"snap", b"shot").expect("put failed");
    let snap = store.snapshot().expect("snapshot failed");
    store.put(b"snap", b"modified").expect("put failed");
    // Snapshot should see the old value.
    assert_eq!(
        snap.get(b"snap").expect("snap get failed").as_deref(),
        Some(b"shot".as_ref())
    );
}

#[test]
fn open_with_redb_explicit() {
    let store = open_with(StoreKind::Redb, temp_dir("redb-explicit")).expect("open_with failed");
    store.put(b"explicit", b"redb").expect("put failed");
    assert_eq!(
        store.get(b"explicit").expect("get failed").as_deref(),
        Some(b"redb".as_ref())
    );
}

#[cfg(feature = "kv-sled")]
#[test]
fn open_with_sled_roundtrip() {
    let store = open_with(StoreKind::Sled, temp_dir("sled")).expect("sled open failed");
    store.put(b"k", b"v").expect("put failed");
    assert_eq!(
        store.get(b"k").expect("get failed").as_deref(),
        Some(b"v".as_ref())
    );
}

#[cfg(feature = "kv-sled")]
#[test]
fn open_with_sled_transaction() {
    let store = open_with(StoreKind::Sled, temp_dir("sled-txn")).expect("sled open failed");
    let mut txn = store.transaction().expect("transaction failed");
    txn.put(b"sledkey", b"sledval").expect("txn put failed");
    txn.commit().expect("commit failed");
    assert_eq!(
        store.get(b"sledkey").expect("get failed").as_deref(),
        Some(b"sledval".as_ref())
    );
}

#[cfg(feature = "kv-fjall")]
#[test]
fn open_with_fjall_roundtrip() {
    let store = open_with(StoreKind::Fjall, temp_dir("fjall")).expect("fjall open failed");
    store.put(b"k", b"v").expect("put failed");
    assert_eq!(
        store.get(b"k").expect("get failed").as_deref(),
        Some(b"v".as_ref())
    );
}

#[cfg(feature = "kv-fjall")]
#[test]
fn open_with_fjall_delete() {
    let store = open_with(StoreKind::Fjall, temp_dir("fjall-del")).expect("fjall open failed");
    store.put(b"key", b"val").expect("put failed");
    assert!(store.contains(b"key").expect("contains failed"));
    store.delete(b"key").expect("delete failed");
    assert!(!store.contains(b"key").expect("contains failed"));
}

#[cfg(feature = "kv-fjall")]
#[test]
fn open_with_fjall_transaction() {
    let store = open_with(StoreKind::Fjall, temp_dir("fjall-txn")).expect("fjall open failed");
    let mut txn = store.transaction().expect("transaction failed");
    txn.put(b"fjallkey", b"fjallval").expect("txn put failed");
    txn.commit().expect("commit failed");
    assert_eq!(
        store.get(b"fjallkey").expect("get failed").as_deref(),
        Some(b"fjallval".as_ref())
    );
}

#[cfg(feature = "kv-fjall")]
#[test]
fn open_with_fjall_snapshot() {
    let store = open_with(StoreKind::Fjall, temp_dir("fjall-snap")).expect("fjall open failed");
    store.put(b"snap", b"shot").expect("put failed");
    let snap = store.snapshot().expect("snapshot failed");
    store.put(b"snap", b"modified").expect("put failed");
    // Snapshot should see the old value.
    assert_eq!(
        snap.get(b"snap").expect("snap get failed").as_deref(),
        Some(b"shot".as_ref())
    );
}

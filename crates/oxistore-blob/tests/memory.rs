//! Integration tests for [`MemoryBlobStore`].

use bytes::Bytes;
use oxistore_blob::{BlobError, BlobStore, MemoryBlobStore};

/// Build a fresh in-memory store for each test.
fn make_store() -> MemoryBlobStore {
    MemoryBlobStore::new()
}

// ---------------------------------------------------------------------------
// put / get round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_get_roundtrip() {
    let store = make_store();
    let payload = Bytes::from("hello world");
    store
        .put("my-key", payload.clone())
        .await
        .expect("put failed");
    let got = store.get("my-key").await.expect("get failed");
    assert_eq!(got, payload);
}

// ---------------------------------------------------------------------------
// Overwrite semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_overwrites_existing() {
    let store = make_store();
    store
        .put("k", Bytes::from("first"))
        .await
        .expect("first put failed");
    store
        .put("k", Bytes::from("second"))
        .await
        .expect("second put failed");
    let got = store.get("k").await.expect("get failed");
    assert_eq!(got.as_ref(), b"second");
}

// ---------------------------------------------------------------------------
// delete removes key
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_removes_key() {
    let store = make_store();
    store
        .put("del-me", Bytes::from("data"))
        .await
        .expect("put failed");
    store.delete("del-me").await.expect("delete failed");
    match store.get("del-me").await {
        Err(BlobError::NotFound(k)) => assert_eq!(k, "del-me"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// head returns correct size
// ---------------------------------------------------------------------------

#[tokio::test]
async fn head_returns_correct_size() {
    let store = make_store();
    let payload = Bytes::from("abcde");
    store.put("sized", payload).await.expect("put failed");
    let meta = store.head("sized").await.expect("head failed");
    assert_eq!(meta.key, "sized");
    assert_eq!(meta.size, 5);
}

// ---------------------------------------------------------------------------
// list with prefix
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_with_prefix() {
    let store = make_store();
    for key in &["a/x", "a/y", "b/z", "a/z"] {
        store.put(key, Bytes::from("v")).await.expect("put failed");
    }
    let mut keys = store.list("a/").await.expect("list failed");
    keys.sort(); // BTreeMap guarantees order, but sort to be explicit.
    assert_eq!(keys, vec!["a/x", "a/y", "a/z"]);
}

#[tokio::test]
async fn list_all_with_empty_prefix() {
    let store = make_store();
    for key in &["one", "two", "three"] {
        store.put(key, Bytes::from("v")).await.expect("put failed");
    }
    let mut keys = store.list("").await.expect("list failed");
    keys.sort();
    assert_eq!(keys, vec!["one", "three", "two"]);
}

// ---------------------------------------------------------------------------
// missing key → BlobError::NotFound
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_missing_key_returns_not_found() {
    let store = make_store();
    match store.get("ghost").await {
        Err(BlobError::NotFound(k)) => assert_eq!(k, "ghost"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn head_missing_key_returns_not_found() {
    let store = make_store();
    match store.head("no-such-key").await {
        Err(BlobError::NotFound(k)) => assert_eq!(k, "no-such-key"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_missing_key_returns_not_found() {
    let store = make_store();
    match store.delete("absent").await {
        Err(BlobError::NotFound(k)) => assert_eq!(k, "absent"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

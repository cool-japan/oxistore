//! Integration tests for [`LocalBlobStore`].

use bytes::Bytes;
use oxistore_blob::{BlobError, BlobStore, LocalBlobStore};
use std::path::PathBuf;

/// Create a unique temporary directory for a single test run so that parallel
/// test executions do not interfere with one another.
fn temp_dir(name: &str) -> PathBuf {
    let base = std::env::temp_dir()
        .join("oxistore-blob-tests")
        .join(format!("{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&base).expect("failed to create temp dir");
    base
}

fn make_store(name: &str) -> LocalBlobStore {
    LocalBlobStore::new(temp_dir(name))
}

// ---------------------------------------------------------------------------
// put / get round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_get_roundtrip() {
    let store = make_store("put_get_roundtrip");
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
    let store = make_store("put_overwrites_existing");
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
    let store = make_store("delete_removes_key");
    store
        .put("del-me", Bytes::from("data"))
        .await
        .expect("put failed");
    store.delete("del-me").await.expect("delete failed");
    match store.get("del-me").await {
        Err(BlobError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// head returns correct size
// ---------------------------------------------------------------------------

#[tokio::test]
async fn head_returns_correct_size() {
    let store = make_store("head_returns_correct_size");
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
    let store = make_store("list_with_prefix");
    for key in &["a/x", "a/y", "b/z", "a/z"] {
        store.put(key, Bytes::from("v")).await.expect("put failed");
    }
    let keys = store.list("a/").await.expect("list failed");
    // Keys returned sorted — just verify the set.
    assert!(keys.contains(&"a/x".to_string()), "missing a/x");
    assert!(keys.contains(&"a/y".to_string()), "missing a/y");
    assert!(keys.contains(&"a/z".to_string()), "missing a/z");
    assert!(!keys.contains(&"b/z".to_string()), "b/z should be excluded");
    assert_eq!(keys.len(), 3);
}

#[tokio::test]
async fn list_all_with_empty_prefix() {
    let store = make_store("list_all_empty_prefix");
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
    let store = make_store("get_missing");
    match store.get("ghost").await {
        Err(BlobError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn head_missing_key_returns_not_found() {
    let store = make_store("head_missing");
    match store.head("no-such-key").await {
        Err(BlobError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_missing_key_returns_not_found() {
    let store = make_store("delete_missing");
    match store.delete("absent").await {
        Err(BlobError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Key validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_key_rejected() {
    let store = make_store("empty_key");
    match store.put("", Bytes::new()).await {
        Err(BlobError::Other(_)) => {}
        other => panic!("expected BlobError::Other for empty key, got {other:?}"),
    }
}

#[tokio::test]
async fn dotdot_key_rejected() {
    let store = make_store("dotdot_key");
    match store.put("../escape", Bytes::new()).await {
        Err(BlobError::Other(_)) => {}
        other => panic!("expected BlobError::Other for '..' key, got {other:?}"),
    }
}

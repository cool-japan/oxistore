//! Integration tests: expose blob storage through the [`oxistore`] facade crate.
//!
//! These tests verify that:
//! 1. [`oxistore::open_blob`] returns a working [`LocalBlobStore`] at the given path.
//! 2. The store returned by the facade supports all core `BlobStore` operations.
//! 3. [`oxistore::blob`] re-exports are usable for construction of in-memory stores.

use bytes::Bytes;
use oxistore::blob::{BlobStore, BlobStoreBuilder, LocalBlobStore, MemoryBlobStore};

// ── helpers ───────────────────────────────────────────────────────────────────

fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "facade_blob_test_{tag}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ))
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// `oxistore::open_blob` returns a `LocalBlobStore` that supports basic put/get.
#[tokio::test]
async fn facade_open_blob_put_get() {
    let dir = temp_dir("open");
    let store = oxistore::open_blob(&dir).expect("open_blob");

    store
        .put("greeting", Bytes::from("hello from facade"))
        .await
        .expect("put");

    let val = store.get("greeting").await.expect("get");
    assert_eq!(val.as_ref(), b"hello from facade");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `blob` module re-exports allow constructing a `MemoryBlobStore` directly.
#[tokio::test]
async fn facade_blob_module_memory_store() {
    let store = MemoryBlobStore::new();

    store.put("key", Bytes::from("value")).await.expect("put");

    let val = store.get("key").await.expect("get");
    assert_eq!(val.as_ref(), b"value");
}

/// `BlobStoreBuilder` from the facade re-export works with capacity limits.
#[tokio::test]
async fn facade_builder_capacity_limit() {
    use oxistore::blob::BlobError;

    let store = BlobStoreBuilder::new().capacity_bytes(100).build_memory();

    // Store up to the limit.
    store
        .put("small", Bytes::from(vec![0u8; 50]))
        .await
        .expect("put within limit");

    // Storing 60 more bytes would exceed the 100-byte limit (50 used + 60 new = 110).
    let err = store
        .put("big", Bytes::from(vec![1u8; 60]))
        .await
        .expect_err("should exceed quota");

    assert!(
        matches!(err, BlobError::QuotaExceeded { .. }),
        "expected QuotaExceeded, got {err:?}"
    );
}

/// `LocalBlobStore` from the facade supports nested keys and `list`.
#[tokio::test]
async fn facade_local_nested_keys_and_list() {
    let dir = temp_dir("nested");
    let store = LocalBlobStore::new(&dir);

    for i in 0..4u8 {
        store
            .put(&format!("dir/sub/file{i}"), Bytes::from(vec![i]))
            .await
            .expect("put nested");
    }

    let keys = store.list("dir/").await.expect("list");
    assert_eq!(keys.len(), 4, "expected 4 nested keys, got {}", keys.len());

    let _ = std::fs::remove_dir_all(&dir);
}

/// `delete` and `head` work correctly on a facade-opened store.
#[tokio::test]
async fn facade_delete_and_head() {
    use oxistore::blob::BlobError;

    let dir = temp_dir("del_head");
    let store = oxistore::open_blob(&dir).expect("open_blob");

    store
        .put("target", Bytes::from("to_be_deleted"))
        .await
        .expect("put");

    // head returns correct size.
    let meta = store.head("target").await.expect("head");
    assert_eq!(meta.size, b"to_be_deleted".len() as u64);

    // delete removes the blob.
    store.delete("target").await.expect("delete");

    let err = store
        .get("target")
        .await
        .expect_err("should be not-found after delete");
    assert!(
        matches!(err, BlobError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// CAS round-trip through the facade-opened local store.
#[tokio::test]
async fn facade_cas_round_trip() {
    let dir = temp_dir("cas");
    let store = oxistore::open_blob(&dir).expect("open_blob");

    let payload = Bytes::from("cas payload via facade");
    let digest = store.put_cas(payload.clone()).await.expect("put_cas");

    let retrieved = store.get_cas(&digest).await.expect("get_cas");
    assert_eq!(retrieved.as_ref(), payload.as_ref());

    let _ = std::fs::remove_dir_all(&dir);
}

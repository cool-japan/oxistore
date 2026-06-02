/// Comprehensive test suite for oxistore-blob.
///
/// Tests cover unicode keys, nested key hierarchies, large blobs, key listing,
/// delete semantics, overwrite, MemoryBlobStore, LocalBlobStore with checksum
/// verification, concurrent access, and empty data.
///
/// All BlobStore methods are async; tests use `#[tokio::test]`.
use std::sync::Arc;

use bytes::Bytes;
use oxistore_blob::{BlobStore, LocalBlobStore, MemoryBlobStore};

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn make_blob_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oxistore_blob_comprehensive_{}_{}",
        name,
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create_dir_all");
    dir
}

// ---------------------------------------------------------------------------
// 1. Unicode key
// ---------------------------------------------------------------------------

/// Keys containing Unicode (Japanese characters) are accepted by LocalBlobStore.
/// validate_key only rejects empty or `..` components.
#[tokio::test]
async fn blob_unicode_key() {
    let dir = make_blob_dir("unicode");
    let store = LocalBlobStore::new(&dir);

    let key = "hello/sekai/blob.bin";
    let data = Bytes::from_static(b"unicode content");

    store.put(key, data.clone()).await.expect("put");
    let got = store.get(key).await.expect("get");
    assert_eq!(got.as_ref(), data.as_ref(), "unicode key round-trip");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ---------------------------------------------------------------------------
// 2. Nested key hierarchy
// ---------------------------------------------------------------------------

/// Keys with multiple `/`-separated segments create nested subdirectories.
#[tokio::test]
async fn blob_nested_key_hierarchy() {
    let dir = make_blob_dir("nested");
    let store = LocalBlobStore::new(&dir);

    let key = "a/b/c/d/nested.bin";
    let data = Bytes::from_static(b"nested");

    store.put(key, data.clone()).await.expect("put");
    let got = store.get(key).await.expect("get");
    assert_eq!(got.as_ref(), data.as_ref(), "nested key round-trip");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ---------------------------------------------------------------------------
// 3. Large blob (1 MiB)
// ---------------------------------------------------------------------------

/// Write 1 048 576 bytes, read back, and verify byte-for-byte equality.
#[tokio::test]
async fn blob_large_blob_1mb() {
    let dir = make_blob_dir("large");
    let store = LocalBlobStore::new(&dir);

    let payload: Vec<u8> = vec![0x42u8; 1_048_576];
    let data = Bytes::from(payload.clone());

    store.put("large.bin", data).await.expect("put");
    let got = store.get("large.bin").await.expect("get");

    assert_eq!(got.len(), 1_048_576, "size preserved");
    assert_eq!(got.as_ref(), payload.as_slice(), "byte-for-byte equal");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ---------------------------------------------------------------------------
// 4. Key listing
// ---------------------------------------------------------------------------

/// Write 5 blobs with distinct keys, list by prefix, verify all 5 appear.
#[tokio::test]
async fn blob_key_listing() {
    let dir = make_blob_dir("listing");
    let store = LocalBlobStore::new(&dir);

    let prefix = "list_test_";
    for i in 1..=5 {
        let key = format!("{prefix}{i}");
        store
            .put(&key, Bytes::from(format!("data{i}")))
            .await
            .expect("put");
    }

    let keys = store.list(prefix).await.expect("list");
    for i in 1..=5 {
        let expected = format!("{prefix}{i}");
        assert!(
            keys.contains(&expected),
            "expected key '{expected}' in listing"
        );
    }
    assert!(
        keys.len() >= 5,
        "listing must return at least 5 keys, got {}",
        keys.len()
    );

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ---------------------------------------------------------------------------
// 5. Delete then missing
// ---------------------------------------------------------------------------

/// After delete, get returns NotFound.
#[tokio::test]
async fn blob_delete_then_missing() {
    let dir = make_blob_dir("delete");
    let store = LocalBlobStore::new(&dir);

    let key = "delete_me.bin";
    store.put(key, Bytes::from("some data")).await.expect("put");

    // Confirm it exists.
    assert!(store.exists(key).await.expect("exists before delete"));

    store.delete(key).await.expect("delete");

    // After deletion, get must return an error.
    let result = store.get(key).await;
    assert!(result.is_err(), "get after delete should return error");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ---------------------------------------------------------------------------
// 6. Overwrite key
// ---------------------------------------------------------------------------

/// Writing to the same key twice keeps only the second value.
#[tokio::test]
async fn blob_overwrite_key() {
    let dir = make_blob_dir("overwrite");
    let store = LocalBlobStore::new(&dir);

    let key = "overwrite.bin";
    store
        .put(key, Bytes::from_static(b"first"))
        .await
        .expect("first put");
    store
        .put(key, Bytes::from_static(b"second"))
        .await
        .expect("second put");

    let got = store.get(key).await.expect("get");
    assert_eq!(got.as_ref(), b"second", "second write must win");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ---------------------------------------------------------------------------
// 7. MemoryBlobStore write/read
// ---------------------------------------------------------------------------

/// MemoryBlobStore::default() (no files) supports put/get.
#[tokio::test]
async fn blob_memory_store_write_read() {
    let store = MemoryBlobStore::default();

    let data = Bytes::from_static(b"hello memory store");
    store.put("mem_key", data.clone()).await.expect("put");
    let got = store.get("mem_key").await.expect("get");

    assert_eq!(got.as_ref(), data.as_ref(), "memory store round-trip");
}

// ---------------------------------------------------------------------------
// 8. Checksum verification passes for CAS key
// ---------------------------------------------------------------------------

/// LocalBlobStore::with_checksum_verification: put_cas then get by hex digest
/// succeeds (checksum matches).
#[tokio::test]
async fn blob_checksum_verification_passes() {
    let dir = make_blob_dir("checksum");
    let store = LocalBlobStore::with_checksum_verification(&dir);

    let data = Bytes::from_static(b"verifiable payload for checksum test");
    let digest = store.put_cas(data.clone()).await.expect("put_cas");

    // Retrieve by the 64-character hex digest key — verification must pass.
    let got = store.get(&digest.to_hex()).await.expect("get by CAS key");
    assert_eq!(
        got.as_ref(),
        data.as_ref(),
        "retrieved data matches written data"
    );

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ---------------------------------------------------------------------------
// 9. Concurrent 4-task writes and reads
// ---------------------------------------------------------------------------

/// Spawn 4 tokio tasks that each independently write and read a unique key.
/// LocalBlobStore is Clone and Send+Sync so it can be shared via Arc.
#[tokio::test]
async fn blob_concurrent_4tasks() {
    let dir = make_blob_dir("concurrent");
    let store = Arc::new(LocalBlobStore::new(&dir));

    let tasks: Vec<_> = (0..4)
        .map(|i| {
            let s = Arc::clone(&store);
            tokio::spawn(async move {
                let key = format!("concurrent_{i}.bin");
                let payload = Bytes::from(vec![i as u8; 1024]);
                s.put(&key, payload.clone()).await.expect("concurrent put");
                let got = s.get(&key).await.expect("concurrent get");
                assert_eq!(
                    got.as_ref(),
                    payload.as_ref(),
                    "task {i}: round-trip mismatch"
                );
            })
        })
        .collect();

    for task in tasks {
        task.await.expect("task panicked");
    }

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ---------------------------------------------------------------------------
// 10. Empty data
// ---------------------------------------------------------------------------

/// Storing b"" and reading back yields an empty slice.
#[tokio::test]
async fn blob_empty_data() {
    let store = MemoryBlobStore::new();

    store
        .put("empty.bin", Bytes::from_static(b""))
        .await
        .expect("put empty");
    let got = store.get("empty.bin").await.expect("get empty");

    assert_eq!(got.len(), 0, "empty data must round-trip as empty");
    assert_eq!(got.as_ref(), b"", "empty slice equality");
}

// ---------------------------------------------------------------------------
// 11. Head metadata after put
// ---------------------------------------------------------------------------

/// head() returns the correct size after a put.
#[tokio::test]
async fn blob_head_after_put() {
    let store = MemoryBlobStore::new();

    let data = Bytes::from(vec![0xABu8; 512]);
    store.put("meta_key", data).await.expect("put");

    let meta = store.head("meta_key").await.expect("head");
    assert_eq!(meta.key, "meta_key");
    assert_eq!(meta.size, 512, "size must match payload");
}

// ---------------------------------------------------------------------------
// 12. List with empty prefix returns all keys
// ---------------------------------------------------------------------------

/// list("") enumerates all stored keys.
#[tokio::test]
async fn blob_list_empty_prefix_returns_all() {
    let store = MemoryBlobStore::new();

    store
        .put("x", Bytes::from_static(b"1"))
        .await
        .expect("put x");
    store
        .put("y", Bytes::from_static(b"2"))
        .await
        .expect("put y");
    store
        .put("z", Bytes::from_static(b"3"))
        .await
        .expect("put z");

    let keys = store.list("").await.expect("list");
    assert!(keys.contains(&"x".to_string()), "must contain x");
    assert!(keys.contains(&"y".to_string()), "must contain y");
    assert!(keys.contains(&"z".to_string()), "must contain z");
    assert_eq!(keys.len(), 3, "exactly 3 keys in store");
}

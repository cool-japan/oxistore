use bytes::Bytes;
use oxistore_blob::{
    BlobError, BlobMeta, BlobStore, BlobStoreBuilder, ChunkedUpload, LocalBlobStore,
    MemoryBlobStore,
};

// ── Previously-existing tests ─────────────────────────────────────────────────

#[tokio::test]
async fn exists_returns_true_for_present() {
    let store = MemoryBlobStore::new();
    store.put("key", Bytes::from("data")).await.expect("put");
    assert!(store.exists("key").await.expect("exists"));
}

#[tokio::test]
async fn exists_returns_false_for_absent() {
    let store = MemoryBlobStore::new();
    assert!(!store.exists("nope").await.expect("exists"));
}

#[tokio::test]
async fn copy_blob() {
    let store = MemoryBlobStore::new();
    store.put("src", Bytes::from("hello")).await.expect("put");
    store.copy("src", "dst").await.expect("copy");

    let data = store.get("dst").await.expect("get");
    assert_eq!(data.as_ref(), b"hello");
    // Source still exists.
    assert!(store.exists("src").await.expect("exists"));
}

#[tokio::test]
async fn rename_blob() {
    let store = MemoryBlobStore::new();
    store.put("old", Bytes::from("data")).await.expect("put");
    store.rename("old", "new").await.expect("rename");

    assert!(!store.exists("old").await.expect("exists"));
    let data = store.get("new").await.expect("get");
    assert_eq!(data.as_ref(), b"data");
}

#[tokio::test]
async fn delete_many() {
    let store = MemoryBlobStore::new();
    store.put("a", Bytes::from("1")).await.expect("put");
    store.put("b", Bytes::from("2")).await.expect("put");
    store.put("c", Bytes::from("3")).await.expect("put");

    store
        .delete_many(&["a", "c", "nonexistent"])
        .await
        .expect("delete_many");

    assert!(!store.exists("a").await.expect("exists"));
    assert!(store.exists("b").await.expect("exists"));
    assert!(!store.exists("c").await.expect("exists"));
}

#[tokio::test]
async fn delete_prefix() {
    let store = MemoryBlobStore::new();
    store.put("logs/a", Bytes::from("1")).await.expect("put");
    store.put("logs/b", Bytes::from("2")).await.expect("put");
    store.put("data/x", Bytes::from("3")).await.expect("put");

    let count = store.delete_prefix("logs/").await.expect("delete_prefix");
    assert_eq!(count, 2);
    assert!(!store.exists("logs/a").await.expect("exists"));
    assert!(store.exists("data/x").await.expect("exists"));
}

#[tokio::test]
async fn put_if_absent_succeeds() {
    let store = MemoryBlobStore::new();
    store
        .put_if_absent("new-key", Bytes::from("data"))
        .await
        .expect("put_if_absent");
    let data = store.get("new-key").await.expect("get");
    assert_eq!(data.as_ref(), b"data");
}

#[tokio::test]
async fn put_if_absent_fails_when_exists() {
    let store = MemoryBlobStore::new();
    store.put("taken", Bytes::from("old")).await.expect("put");

    let result = store.put_if_absent("taken", Bytes::from("new")).await;
    assert!(result.is_err());
    // Original data unchanged.
    let data = store.get("taken").await.expect("get");
    assert_eq!(data.as_ref(), b"old");
}

// ── QuotaExceeded ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn quota_exceeded_when_store_full() {
    // Capacity of 10 bytes.
    let store = BlobStoreBuilder::new().capacity_bytes(10).build_memory();

    // A 10-byte write should succeed.
    store
        .put("key1", Bytes::from("0123456789"))
        .await
        .expect("first put should succeed");

    // A second write would push total over the limit.
    let err = store
        .put("key2", Bytes::from("x"))
        .await
        .expect_err("expected quota error");

    assert!(
        matches!(err, BlobError::QuotaExceeded { .. }),
        "expected QuotaExceeded, got {err:?}"
    );
}

#[tokio::test]
async fn quota_allows_overwrite_within_limit() {
    // Capacity of 10 bytes.
    let store = BlobStoreBuilder::new().capacity_bytes(10).build_memory();

    store
        .put("k", Bytes::from("hello"))
        .await
        .expect("first put");

    // Overwrite with a shorter value — should stay within quota.
    store
        .put("k", Bytes::from("hi"))
        .await
        .expect("overwrite should succeed");

    let data = store.get("k").await.expect("get");
    assert_eq!(data.as_ref(), b"hi");
}

// ── AlreadyExists ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn already_exists_returned_by_put_if_absent() {
    let store = MemoryBlobStore::new();
    store.put("existing", Bytes::from("v1")).await.expect("put");

    let err = store
        .put_if_absent("existing", Bytes::from("v2"))
        .await
        .expect_err("expected AlreadyExists");

    assert!(
        matches!(err, BlobError::AlreadyExists(_)),
        "expected AlreadyExists, got {err:?}"
    );
}

// ── ChunkedUpload ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn chunked_upload_assembles_correctly() {
    let mut upload = ChunkedUpload::new();
    upload.push_chunk(b"foo".as_slice());
    upload.push_chunk(b"bar".as_slice());
    upload.push_chunk(b"baz".as_slice());
    assert_eq!(upload.assemble(), b"foobarbaz");
}

#[tokio::test]
async fn chunked_upload_empty() {
    let upload = ChunkedUpload::new();
    assert_eq!(upload.assemble(), b"");
}

#[tokio::test]
async fn put_chunked_equals_one_shot_put() {
    let store = MemoryBlobStore::new();

    // Build a large payload via chunked upload.
    let chunk = vec![0x42u8; 1024];
    let mut upload = ChunkedUpload::new();
    for _ in 0..64 {
        upload.push_chunk(chunk.clone());
    }
    let expected: Vec<u8> = vec![0x42u8; 1024 * 64];

    store.put_chunked("big", upload).await.expect("put_chunked");

    let data = store.get("big").await.expect("get");
    assert_eq!(data.as_ref(), expected.as_slice());
}

// ── list_meta ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_meta_returns_correct_sizes() {
    let store = MemoryBlobStore::new();
    store.put("a/x", Bytes::from("hello")).await.expect("put");
    store.put("a/y", Bytes::from("world!")).await.expect("put");
    store.put("b/z", Bytes::from("other")).await.expect("put");

    let metas = store.list_meta("a/").await.expect("list_meta");
    assert_eq!(metas.len(), 2);

    let x = metas.iter().find(|m| m.key == "a/x").expect("a/x");
    let y = metas.iter().find(|m| m.key == "a/y").expect("a/y");
    assert_eq!(x.size, 5, "a/x should be 5 bytes");
    assert_eq!(y.size, 6, "a/y should be 6 bytes");
}

// ── list_meta_page ────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_meta_page_splits_correctly() {
    let store = MemoryBlobStore::new();
    for i in 0u8..10 {
        store
            .put(
                &format!("page/{i:02}"),
                Bytes::from(vec![i; i as usize + 1]),
            )
            .await
            .expect("put");
    }

    // First page — no continuation token, limit 4.
    let page1 = store.list_meta_page("page/", None, 4).await.expect("page1");
    assert_eq!(page1.len(), 4);
    assert_eq!(page1[0].key, "page/00");
    assert_eq!(page1[3].key, "page/03");

    // Second page — continue after last key of page1.
    let last_key = page1.last().expect("non-empty").key.clone();
    let page2 = store
        .list_meta_page("page/", Some(&last_key), 4)
        .await
        .expect("page2");
    assert_eq!(page2.len(), 4);
    assert_eq!(page2[0].key, "page/04");
    assert_eq!(page2[3].key, "page/07");

    // Third page — last two entries.
    let last_key2 = page2.last().expect("non-empty").key.clone();
    let page3 = store
        .list_meta_page("page/", Some(&last_key2), 4)
        .await
        .expect("page3");
    assert_eq!(page3.len(), 2);
    assert_eq!(page3[0].key, "page/08");
    assert_eq!(page3[1].key, "page/09");
}

// ── delete_if_matches ─────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_if_matches_deletes_on_matching_digest() {
    let store = MemoryBlobStore::new();
    let data = Bytes::from("delete me");
    store.put("k", data.clone()).await.expect("put");

    let digest = oxistore_blob::sha256(&data);
    let deleted = store
        .delete_if_matches("k", &digest)
        .await
        .expect("delete_if_matches");

    assert!(deleted, "should have deleted the blob");
    assert!(!store.exists("k").await.expect("exists"));
}

#[tokio::test]
async fn delete_if_matches_preserves_on_wrong_digest() {
    let store = MemoryBlobStore::new();
    store.put("k", Bytes::from("keep me")).await.expect("put");

    // Compute digest of *different* data.
    let wrong_digest = oxistore_blob::sha256(b"not the same");
    let deleted = store
        .delete_if_matches("k", &wrong_digest)
        .await
        .expect("delete_if_matches");

    assert!(!deleted, "should not have deleted the blob");
    assert!(store.exists("k").await.expect("exists"));
}

#[tokio::test]
async fn delete_if_matches_returns_false_for_missing_key() {
    let store = MemoryBlobStore::new();
    let digest = oxistore_blob::sha256(b"anything");
    let deleted = store
        .delete_if_matches("nonexistent", &digest)
        .await
        .expect("delete_if_matches");
    assert!(!deleted);
}

// ── BlobStoreBuilder ──────────────────────────────────────────────────────────

#[tokio::test]
async fn builder_build_memory_no_limit() {
    let store = BlobStoreBuilder::new().build_memory();
    // Should succeed without any capacity error.
    store
        .put("any", Bytes::from(vec![0u8; 10_000]))
        .await
        .expect("put should succeed without limit");
}

// ── BlobMeta field tests ──────────────────────────────────────────────────────

/// `BlobMeta` returned by `head` must expose `content_type: None` and
/// `checksum: None` for blobs stored via plain `put`.
#[tokio::test]
async fn blob_meta_has_content_type_field() {
    let store = MemoryBlobStore::new();
    store.put("k", Bytes::from("data")).await.expect("put");

    let meta: BlobMeta = store.head("k").await.expect("head");
    assert!(
        meta.content_type.is_none(),
        "content_type should be None for a plain put"
    );
}

#[tokio::test]
async fn blob_meta_has_checksum_field() {
    let store = MemoryBlobStore::new();
    store.put("k", Bytes::from("hello")).await.expect("put");

    let meta: BlobMeta = store.head("k").await.expect("head");
    assert!(
        meta.checksum.is_none(),
        "checksum should be None for a plain put (non-CAS)"
    );
}

/// `LocalBlobStore::with_checksum_verification` is constructible and behaves
/// like a normal store for ordinary (non-CAS) keys.
#[tokio::test]
async fn local_store_with_checksum_verification_plain_key() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-blob-csv-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let store = LocalBlobStore::with_checksum_verification(&dir);

    store
        .put("plain-key", Bytes::from("some data"))
        .await
        .expect("put");
    let data = store.get("plain-key").await.expect("get");
    assert_eq!(data.as_ref(), b"some data");

    // Cleanup.
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

/// `LocalBlobStore::with_checksum_verification` verifies the SHA-256 when the
/// key is a 64-character hex digest (CAS key).
#[tokio::test]
async fn local_store_checksum_verification_on_cas_key() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-blob-csv-cas-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let store = LocalBlobStore::with_checksum_verification(&dir);

    let data = Bytes::from("verifiable content");
    let digest = store.put_cas(data.clone()).await.expect("put_cas");

    // Getting by the CAS key should succeed (digest matches).
    let got = store.get(&digest.to_hex()).await.expect("get by CAS key");
    assert_eq!(got.as_ref(), data.as_ref());

    // Cleanup.
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

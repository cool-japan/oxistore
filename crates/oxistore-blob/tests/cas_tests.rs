//! Integration tests for content-addressed storage (CAS) on `MemoryBlobStore`.

use bytes::Bytes;
use oxistore_blob::{sha256, BlobError, BlobStore, Digest, MemoryBlobStore};

// ── 1. Deduplication ─────────────────────────────────────────────────────────

/// Same content put twice via `put_cas` returns the same `Digest` and stores
/// only a single entry (verified by `list`).
#[tokio::test]
async fn put_cas_deduplication() {
    let store = MemoryBlobStore::new();
    let data = Bytes::from_static(b"deduplicated content");

    let d1 = store.put_cas(data.clone()).await.expect("first put_cas");
    let d2 = store.put_cas(data.clone()).await.expect("second put_cas");

    // Same content -> same digest.
    assert_eq!(d1, d2, "digest must be identical for identical content");

    // Only one entry in the store (the hex key).
    let keys = store.list("").await.expect("list");
    assert_eq!(keys.len(), 1, "dedup: only one key in store, got {keys:?}");
    assert_eq!(keys[0], d1.to_hex());
}

// ── 2. CAS round-trip ────────────────────────────────────────────────────────

/// `put_cas` followed by `get_cas` returns the original data unchanged.
#[tokio::test]
async fn cas_round_trip() {
    let store = MemoryBlobStore::new();
    let original = Bytes::from_static(b"hello, content-addressed world");

    let digest = store.put_cas(original.clone()).await.expect("put_cas");
    let retrieved = store.get_cas(&digest).await.expect("get_cas");

    assert_eq!(retrieved, original, "round-trip data mismatch");
}

// ── 3. Integrity check ───────────────────────────────────────────────────────

/// If the stored bytes are tampered with directly via `put`, `get_cas` detects
/// the corruption and returns `ChecksumMismatch`.
#[tokio::test]
async fn get_cas_detects_tampering() {
    let store = MemoryBlobStore::new();
    let data = Bytes::from_static(b"tamper-me");

    let digest = store.put_cas(data).await.expect("put_cas");
    let hex_key = digest.to_hex();

    // Overwrite the stored blob with corrupted bytes via the raw put interface.
    store
        .put(&hex_key, Bytes::from_static(b"corrupted!"))
        .await
        .expect("raw put for tampering");

    let result = store.get_cas(&digest).await;
    assert!(result.is_err(), "expected error on tampered blob");

    match result {
        Err(BlobError::ChecksumMismatch(msg)) => {
            // Message should mention the expected digest.
            assert!(
                msg.contains(&hex_key[..8]),
                "message should mention digest; got: {msg}"
            );
        }
        Err(other) => panic!("expected ChecksumMismatch, got {other}"),
        Ok(_) => panic!("expected Err, got Ok"),
    }
}

// ── 4. Streaming round-trip ──────────────────────────────────────────────────

/// `put_streaming` stores the data and returns a digest equal to `sha256(data)`.
/// Subsequent `get_cas` with that digest retrieves the original data.
///
/// `std::io::Cursor<Vec<u8>>` implements `tokio::io::AsyncRead` in tokio 1.x.
#[tokio::test]
async fn streaming_round_trip() {
    let store = MemoryBlobStore::new();
    let data: Vec<u8> = (0u8..=255).cycle().take(4096).collect();

    // std::io::Cursor<Vec<u8>> implements tokio::io::AsyncRead.
    let cursor = std::io::Cursor::new(data.clone());
    let digest = store.put_streaming(cursor).await.expect("put_streaming");

    let expected_digest = sha256(&data);
    assert_eq!(
        digest, expected_digest,
        "streaming digest must match one-shot sha256"
    );

    let retrieved = store.get_cas(&digest).await.expect("get_cas");
    assert_eq!(
        retrieved.as_ref(),
        data.as_slice(),
        "round-trip data mismatch"
    );
}

// ── 5. Not found ─────────────────────────────────────────────────────────────

/// `get_cas` with a digest that was never stored returns `NotFound`.
#[tokio::test]
async fn get_cas_not_found() {
    let store = MemoryBlobStore::new();
    let phantom_digest = sha256(b"this was never stored");

    let result = store.get_cas(&phantom_digest).await;
    assert!(
        matches!(result, Err(BlobError::NotFound(_))),
        "expected NotFound, got {result:?}"
    );
}

// ── 6. Digest display / parse ────────────────────────────────────────────────

/// `Digest::to_hex()` followed by `Digest::from_hex()` round-trips losslessly.
#[tokio::test]
async fn digest_display_parse_round_trip() {
    let data = b"display and parse test";
    let digest = sha256(data);

    let hex = digest.to_hex();
    assert_eq!(hex.len(), 64, "hex digest must be 64 chars");
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "all hex chars");

    let restored = Digest::from_hex(&hex).expect("from_hex");
    assert_eq!(digest, restored, "from_hex must round-trip");

    let display = format!("{digest}");
    assert_eq!(display, hex, "Display must match to_hex()");
}

// ── 7. Cross-store address stability ────────────────────────────────────────

/// The same content put into two independent `MemoryBlobStore` instances
/// produces the same `Digest`.
#[tokio::test]
async fn cross_store_digest_stability() {
    let store_a = MemoryBlobStore::new();
    let store_b = MemoryBlobStore::new();
    let data = Bytes::from_static(b"cross-store-stability-test");

    let d_a = store_a.put_cas(data.clone()).await.expect("put_cas A");
    let d_b = store_b.put_cas(data.clone()).await.expect("put_cas B");

    assert_eq!(
        d_a, d_b,
        "identical content must yield the same digest in different stores"
    );
}

// ── 8. exists_cas ────────────────────────────────────────────────────────────

/// `exists_cas` returns `true` after a `put_cas` and `false` for an unknown digest.
#[tokio::test]
async fn exists_cas_present_and_absent() {
    let store = MemoryBlobStore::new();
    let data = Bytes::from_static(b"exists check content");

    let absent_digest = sha256(b"definitely not stored");
    assert!(
        !store
            .exists_cas(&absent_digest)
            .await
            .expect("exists_cas absent"),
        "should be false before any put"
    );

    let digest = store.put_cas(data).await.expect("put_cas");
    assert!(
        store.exists_cas(&digest).await.expect("exists_cas present"),
        "should be true after put_cas"
    );
}

// ── 9. get_verified is identical to get_cas ──────────────────────────────────

/// `get_verified` is an alias for `get_cas`; it must return the same bytes.
#[tokio::test]
async fn get_verified_alias() {
    let store = MemoryBlobStore::new();
    let data = Bytes::from_static(b"verified read");

    let digest = store.put_cas(data.clone()).await.expect("put_cas");
    let verified = store.get_verified(&digest).await.expect("get_verified");
    assert_eq!(verified, data, "get_verified must return original data");
}

// ── 10. Different content -> different digest ────────────────────────────────

/// Two blobs with distinct content produce distinct digests.
#[tokio::test]
async fn different_content_different_digest() {
    let store = MemoryBlobStore::new();

    let d1 = store
        .put_cas(Bytes::from_static(b"content A"))
        .await
        .expect("put_cas A");
    let d2 = store
        .put_cas(Bytes::from_static(b"content B"))
        .await
        .expect("put_cas B");

    assert_ne!(d1, d2, "distinct content must produce distinct digests");
    assert_eq!(store.list("").await.expect("list").len(), 2);
}

// ── 11. Streaming deduplication ──────────────────────────────────────────────

/// `put_streaming` with the same content twice deduplicates.
#[tokio::test]
async fn put_streaming_deduplication() {
    let store = MemoryBlobStore::new();
    let data: Vec<u8> = b"streaming dedup content".to_vec();

    let d1 = store
        .put_streaming(std::io::Cursor::new(data.clone()))
        .await
        .expect("first put_streaming");
    let d2 = store
        .put_streaming(std::io::Cursor::new(data.clone()))
        .await
        .expect("second put_streaming");

    assert_eq!(d1, d2, "streaming dedup: same content -> same digest");
    let keys = store.list("").await.expect("list");
    assert_eq!(
        keys.len(),
        1,
        "streaming dedup: one key expected, got {keys:?}"
    );
}

// ── 12. Empty blob CAS ───────────────────────────────────────────────────────

/// The empty blob has a well-known SHA-256 and can be stored and retrieved.
#[tokio::test]
async fn empty_blob_cas() {
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    let store = MemoryBlobStore::new();
    let digest = store.put_cas(Bytes::new()).await.expect("put_cas empty");

    assert_eq!(digest.to_hex(), EMPTY_SHA256, "empty blob sha256 mismatch");

    let retrieved = store.get_cas(&digest).await.expect("get_cas empty");
    assert!(retrieved.is_empty(), "empty blob must round-trip as empty");
}

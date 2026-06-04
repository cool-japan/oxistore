//! Integration tests: cache frequently accessed blobs in LRU/ARC cache via
//! [`oxistore_cache::BlobCache`].
//!
//! These tests verify that:
//! 1. `BlobCache` transparently forwards `put` / `get` / `delete` to the inner
//!    [`BlobStore`] while serving repeated `get` calls from the in-memory LRU.
//! 2. Hit/miss statistics are updated correctly.
//! 3. A `put` (overwrite) invalidates the previously cached value so stale
//!    data is never served.
//! 4. A `delete` evicts the cache entry so subsequent `get` returns `NotFound`.
//! 5. `BlobCache` correctly implements the full `BlobStore` trait, including
//!    `head`, `list`, and higher-level helpers (`exists`, `put_if_absent`, etc.).

use bytes::Bytes;
use oxistore_blob::{BlobError, BlobStore, MemoryBlobStore};
use oxistore_cache::BlobCache;

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_cache(capacity: usize) -> BlobCache<MemoryBlobStore> {
    BlobCache::new(MemoryBlobStore::new(), capacity)
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Basic put/get round-trip through the cache.
#[tokio::test]
async fn cache_put_get_round_trip() {
    let cache = make_cache(10);

    cache.put("hello", Bytes::from("world")).await.expect("put");

    let val = cache.get("hello").await.expect("get");
    assert_eq!(val.as_ref(), b"world");
}

/// First `get` is a miss (served from inner store); second `get` is a hit
/// (served from the in-memory LRU).
#[tokio::test]
async fn cache_hit_miss_counts() {
    let inner = MemoryBlobStore::new();
    inner
        .put("k", Bytes::from("v"))
        .await
        .expect("seed inner store");

    let cache = BlobCache::new(inner, 10);
    let stats = cache.stats();

    // Cold get — should be a miss.
    let _first = cache.get("k").await.expect("first get");
    assert_eq!(stats.hits(), 0);
    assert_eq!(stats.misses(), 1);

    // Second get — should hit the LRU.
    let _second = cache.get("k").await.expect("second get");
    assert_eq!(stats.hits(), 1);
    assert_eq!(stats.misses(), 1);

    // Hit rate should be 0.5.
    assert!(
        (stats.hit_rate() - 0.5).abs() < f64::EPSILON,
        "expected 50% hit rate, got {}",
        stats.hit_rate()
    );
}

/// `put` invalidates the cached value so the next `get` reflects the update.
#[tokio::test]
async fn cache_put_invalidates_stale_entry() {
    let cache = make_cache(10);

    cache.put("key", Bytes::from("v1")).await.expect("put v1");

    // Warm the cache.
    let _ = cache.get("key").await.expect("get v1");

    // Overwrite with new value — cache entry for "key" must be evicted.
    cache.put("key", Bytes::from("v2")).await.expect("put v2");

    // Next get must return the fresh value.
    let val = cache.get("key").await.expect("get v2");
    assert_eq!(val.as_ref(), b"v2", "stale cached value served after put");
}

/// `delete` evicts the cache entry and subsequent `get` returns `NotFound`.
#[tokio::test]
async fn cache_delete_evicts_and_returns_not_found() {
    let cache = make_cache(10);

    cache.put("blob", Bytes::from("data")).await.expect("put");
    // Warm cache.
    let _ = cache.get("blob").await.expect("first get");

    cache.delete("blob").await.expect("delete");

    let err = cache.get("blob").await.expect_err("should be not-found");
    assert!(
        matches!(err, BlobError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

/// `head` is always forwarded to the inner store; metadata is not cached.
#[tokio::test]
async fn cache_head_forwarded_to_inner() {
    let cache = make_cache(10);
    let payload = Bytes::from("metadata test payload");
    let expected_size = payload.len() as u64;

    cache.put("m", payload).await.expect("put");

    let meta = cache.head("m").await.expect("head");
    assert_eq!(meta.size, expected_size);
    assert_eq!(meta.key, "m");
}

/// `list` is forwarded to the inner store.
#[tokio::test]
async fn cache_list_forwarded_to_inner() {
    let cache = make_cache(20);

    for i in 0..5u8 {
        cache
            .put(&format!("prefix/item{i}"), Bytes::from(vec![i]))
            .await
            .expect("put");
    }

    let keys = cache.list("prefix/").await.expect("list");
    assert_eq!(keys.len(), 5);
    for key in &keys {
        assert!(key.starts_with("prefix/"), "unexpected key: {key}");
    }
}

/// `exists` returns `true` for a present key and `false` for an absent key.
#[tokio::test]
async fn cache_exists() {
    let cache = make_cache(5);

    cache.put("present", Bytes::from("y")).await.expect("put");

    assert!(cache.exists("present").await.expect("exists present"));
    assert!(!cache.exists("absent").await.expect("exists absent"));
}

/// `put_if_absent` returns `AlreadyExists` when the key is already cached.
#[tokio::test]
async fn cache_put_if_absent() {
    let cache = make_cache(5);

    cache
        .put_if_absent("once", Bytes::from("first"))
        .await
        .expect("first put_if_absent");

    let err = cache
        .put_if_absent("once", Bytes::from("second"))
        .await
        .expect_err("second put_if_absent should fail");

    assert!(
        matches!(err, BlobError::AlreadyExists(_)),
        "expected AlreadyExists, got {err:?}"
    );
}

/// Cache eviction respects capacity: after capacity is reached, LRU entries
/// are evicted and their data is re-fetched from the inner store on next access.
#[tokio::test]
async fn cache_capacity_eviction() {
    // Capacity of 2 entries.
    let cache = make_cache(2);

    cache.put("a", Bytes::from("aaa")).await.expect("put a");
    cache.put("b", Bytes::from("bbb")).await.expect("put b");

    // Warm: both a and b are now in cache.
    let _ = cache.get("a").await.expect("get a");
    let _ = cache.get("b").await.expect("get b");

    // Adding "c" evicts the LRU entry ("a" or "b").
    cache.put("c", Bytes::from("ccc")).await.expect("put c");

    // Verify "c" is retrievable.
    let vc = cache.get("c").await.expect("get c");
    assert_eq!(vc.as_ref(), b"ccc");

    // The two remaining entries should still be retrievable from the inner store
    // even if they were evicted from the LRU.
    let va = cache.get("a").await.expect("get a after eviction");
    assert_eq!(va.as_ref(), b"aaa");

    let vb = cache.get("b").await.expect("get b after eviction");
    assert_eq!(vb.as_ref(), b"bbb");
}

/// `delete_many` removes multiple keys from both cache and inner store.
#[tokio::test]
async fn cache_delete_many() {
    let cache = make_cache(10);

    for i in 0..5u8 {
        cache
            .put(&format!("del{i}"), Bytes::from(vec![i]))
            .await
            .expect("put");
    }

    cache
        .delete_many(&["del0", "del1", "del2"])
        .await
        .expect("delete_many");

    for i in 0u8..3 {
        let err = cache
            .get(&format!("del{i}"))
            .await
            .expect_err("should be deleted");
        assert!(matches!(err, BlobError::NotFound(_)));
    }

    // del3 and del4 should still exist.
    for i in 3u8..5 {
        let _ = cache
            .get(&format!("del{i}"))
            .await
            .expect("should still exist");
    }
}

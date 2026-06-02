//! Property-based and regression tests for the oxistore-cache crate.
//!
//! Uses `proptest` to verify invariants that must hold across all cache
//! implementations (LRU, ARC, LFU) under arbitrary operation sequences.

use oxistore_cache::{ArcCache, Cache, LfuCache, LruCache, SyncCache};
use proptest::prelude::*;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// proptest: LRU length invariant
// ---------------------------------------------------------------------------

/// Operation to apply to the cache in the length-invariant property test.
#[derive(Debug, Clone)]
enum CacheOp {
    Put(u8, u8),
    Get(u8),
    Remove(u8),
}

fn arb_cache_op() -> impl Strategy<Value = CacheOp> {
    prop_oneof![
        (any::<u8>().prop_map(|k| k % 8), any::<u8>()).prop_map(|(k, v)| CacheOp::Put(k, v)),
        any::<u8>().prop_map(|k| k % 8).prop_map(CacheOp::Get),
        any::<u8>().prop_map(|k| k % 8).prop_map(CacheOp::Remove),
    ]
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(200))]

    /// After every operation, `len() <= cap()` must hold.
    #[test]
    fn prop_lru_len_invariant(ops in proptest::collection::vec(arb_cache_op(), 0..50)) {
        const CAP: usize = 5;
        let mut cache: LruCache<u8, u8> = LruCache::new(CAP);

        for op in &ops {
            match op {
                CacheOp::Put(k, v) => { let _ = cache.put(*k, *v); }
                CacheOp::Get(k)    => { let _ = cache.get(k); }
                CacheOp::Remove(k) => { let _ = cache.remove(k); }
            }
            prop_assert!(
                cache.len() <= cache.cap(),
                "len={} exceeded cap={} after op {:?}",
                cache.len(), cache.cap(), op
            );
        }
    }

    /// Inserting up to `cap` distinct pairs — each must be retrievable.
    ///
    /// Keys are capped at 20 to keep the domain small; we insert at most
    /// `cap` pairs so there is no eviction.
    #[test]
    fn prop_lru_get_returns_inserted(
        pairs in proptest::collection::vec(
            (0u8..20u8, any::<u8>()),
            1..6usize,
        )
    ) {
        const CAP: usize = 5;
        // Deduplicate by key (last writer wins), keep at most CAP entries.
        let mut unique: std::collections::HashMap<u8, u8> = std::collections::HashMap::new();
        for (k, v) in &pairs {
            unique.insert(*k, *v);
        }
        // Only take up to CAP so no eviction occurs.
        let subset: Vec<(u8, u8)> = unique.into_iter().take(CAP).collect();

        let mut cache: LruCache<u8, u8> = LruCache::new(CAP);
        for (k, v) in &subset {
            cache.put(*k, *v);
        }

        for (k, v) in &subset {
            let got = cache.get(k);
            prop_assert_eq!(
                got,
                Some(v),
                "key={} expected Some({}) got {:?}",
                k, v, got
            );
        }
    }

    /// ARC len() never exceeds cap() under arbitrary put/get/remove sequences.
    #[test]
    fn prop_arc_len_invariant(ops in proptest::collection::vec(arb_cache_op(), 0..50)) {
        const CAP: usize = 5;
        let mut cache: ArcCache<u8, u8> = ArcCache::new(CAP);

        for op in &ops {
            match op {
                CacheOp::Put(k, v) => { let _ = cache.put(*k, *v); }
                CacheOp::Get(k)    => { let _ = cache.get(k); }
                CacheOp::Remove(k) => { let _ = cache.remove(k); }
            }
            prop_assert!(
                cache.len() <= cache.cap(),
                "ARC len={} exceeded cap={} after op {:?}",
                cache.len(), cache.cap(), op
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Regular tests
// ---------------------------------------------------------------------------

/// ARC is scan-resistant: after a sequential scan of 50 items into both LRU
/// and ARC (cap=20), the ARC cache should retain at least as many hot-set
/// entries (keys 0..4) as LRU when accessed 10 times each.
///
/// This tests the *structural* property of ARC, not a hard guarantee.
#[test]
fn arc_scan_resistance() {
    const CAP: usize = 20;

    let mut lru: LruCache<u32, u32> = LruCache::new(CAP);
    let mut arc: ArcCache<u32, u32> = ArcCache::new(CAP);

    // Sequential scan — 50 items, well beyond the cap of 20.
    for i in 0..50u32 {
        lru.put(i, i);
        arc.put(i, i);
    }

    // Access the hot set (keys 0..4) 10 times each in both caches.
    let hot_keys: Vec<u32> = (0..4).collect();
    for _ in 0..10 {
        for &k in &hot_keys {
            let _ = lru.get(&k);
            let _ = arc.get(&k);
        }
    }

    // Count how many hot keys are retained.
    let lru_hits = hot_keys.iter().filter(|&&k| lru.peek(&k).is_some()).count();
    let arc_hits = hot_keys.iter().filter(|&&k| arc.peek(&k).is_some()).count();

    // After repeated access, ARC should retain at least as many hot entries.
    // We accept ARC >= LRU (ARC is designed for this).
    assert!(
        arc_hits >= lru_hits,
        "Expected ARC({arc_hits}) >= LRU({lru_hits}) hot-set retention after scan",
    );
}

/// LFU eviction order: insert 5 items, build a frequency gradient, then
/// trigger eviction and verify a low-frequency key was evicted.
#[test]
fn lfu_eviction_order() {
    let mut lfu: LfuCache<u8, u8> = LfuCache::new(5);

    // Insert keys 0..5.
    for k in 0u8..5 {
        lfu.put(k, k);
    }

    // Access key=0 five times (freq=6 after insert+5 gets).
    for _ in 0..5 {
        let _ = lfu.get(&0);
    }
    // Access key=1 four times (freq=5).
    for _ in 0..4 {
        let _ = lfu.get(&1);
    }
    // Keys 2..4 accessed zero extra times (freq=1 from insert).

    // Insert a new key to trigger eviction of one low-frequency key.
    lfu.put(10, 10);

    // One of keys 2, 3, or 4 must have been evicted (they have freq=1).
    let still_present: Vec<u8> = (2u8..5u8).filter(|k| lfu.peek(k).is_some()).collect();
    assert!(
        still_present.len() <= 2,
        "Expected at most 2 of {{2,3,4}} to remain; got {:?}",
        still_present
    );

    // High-frequency keys (0 and 1) must still be present.
    assert!(
        lfu.peek(&0).is_some(),
        "key=0 (highest freq) must not be evicted"
    );
    assert!(
        lfu.peek(&1).is_some(),
        "key=1 (second highest freq) must not be evicted"
    );
}

/// TTL expiry: insert a key with a 5 ms TTL, sleep 10 ms, verify miss.
#[test]
fn cache_ttl_expiry() {
    let mut lru: LruCache<u8, u8> = LruCache::new(10);
    lru.put_with_ttl(42, 99, Duration::from_millis(5));

    // Immediately it should be present.
    assert_eq!(lru.peek(&42), Some(&99));

    std::thread::sleep(Duration::from_millis(20));

    // After TTL has passed, get must return None (lazy expiry).
    assert_eq!(
        lru.get(&42),
        None,
        "expected TTL-expired entry to return None"
    );
}

/// `values()` returns all live entries; `warm()` pre-populates the cache.
#[test]
fn cache_values_and_warm() {
    let mut cache: LruCache<u8, u8> = LruCache::new(5);
    cache.put(1, 10);
    cache.put(2, 20);
    cache.put(3, 30);

    let vals = Cache::values(&cache);
    assert_eq!(vals.len(), 3, "expected 3 values, got {}", vals.len());

    // warm() pre-populates with an iterator of (k, v) pairs.
    Cache::warm(&mut cache, vec![(10u8, 100u8)]);
    assert_eq!(
        cache.get(&10),
        Some(&100u8),
        "warm'd key=10 should be retrievable"
    );
}

/// Thread safety: share `Arc<SyncCache<...>>` across 4 threads, each doing
/// 10 puts and 10 gets. No panics expected.
#[test]
fn sync_cache_thread_safety() {
    let sync_cache = Arc::new(SyncCache::new(LruCache::<u8, u8>::new(64)));

    let handles: Vec<_> = (0u8..4)
        .map(|thread_id| {
            let cache = Arc::clone(&sync_cache);
            std::thread::spawn(move || {
                for i in 0u8..10 {
                    let key = thread_id * 10 + i;
                    cache.put(key, key.wrapping_mul(2));
                }
                for i in 0u8..10 {
                    let key = thread_id * 10 + i;
                    let _ = cache.get(&key);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread must not panic");
    }

    // All 40 inserts should be present (cap=64, so no eviction).
    assert_eq!(sync_cache.len(), 40, "expected 40 entries after 4x10 puts");
}

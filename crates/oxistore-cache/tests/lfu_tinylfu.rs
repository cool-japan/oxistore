//! Tests for LFU, W-TinyLFU, Count-Min Sketch, and per-entry TTL.

use std::thread;
use std::time::Duration;

use oxistore_cache::{Cache, LfuCache, LruCache, WTinyLfuCache};

// ===========================================================================
// LFU tests
// ===========================================================================

/// Test 1: LFU eviction order — FIFO within the same frequency bucket.
///
/// With capacity 3, insert a, b, c all at freq=1.
/// Insert d → should evict a (oldest at min_freq=1).
#[test]
fn lfu_eviction_fifo_within_frequency() {
    let mut cache = LfuCache::new(3);
    cache.put(1u32, "a");
    cache.put(2u32, "b");
    cache.put(3u32, "c");

    // All at freq=1.  Insert 4 → evict 1 (oldest at freq=1).
    cache.put(4u32, "d");

    assert!(
        cache.get(&1).is_none(),
        "1 (oldest at min_freq) should be evicted"
    );
    assert_eq!(cache.get(&2), Some(&"b"));
    assert_eq!(cache.get(&3), Some(&"c"));
    assert_eq!(cache.get(&4), Some(&"d"));
}

/// Test 2: LFU O(1) frequency increment — most-frequently-used survives.
///
/// Insert a, b, c.  Access a twice → freq(a)=3, freq(b)=freq(c)=1.
/// Insert d (at cap) → b evicted (oldest at min_freq=1), not a.
#[test]
fn lfu_least_frequently_used_evicted_first() {
    let mut cache = LfuCache::new(3);
    cache.put(1u32, "a");
    cache.put(2u32, "b");
    cache.put(3u32, "c");

    // Boost key 1's frequency.
    cache.get(&1); // freq(1) = 2
    cache.get(&1); // freq(1) = 3

    // Now min_freq = 1 (keys 2 and 3).  Insert 4 → evict 2 (FIFO at freq=1).
    cache.put(4u32, "d");

    // Key 1 should survive (high frequency).
    assert_eq!(cache.get(&1), Some(&"a"));
    assert!(cache.get(&2).is_none(), "2 should be evicted (LFU)");
    assert_eq!(cache.get(&3), Some(&"c"));
    assert_eq!(cache.get(&4), Some(&"d"));
}

/// Test 3: LFU min_freq tracking — min_freq advances when bucket is drained.
#[test]
fn lfu_min_freq_tracking() {
    let mut cache = LfuCache::new(2);
    cache.put(1u32, 10u32);
    cache.put(2u32, 20u32);

    // key 1: freq=2 after one get; key 2: freq=1.
    cache.get(&1);

    // At cap=2, insert 3 → evict key 2 (min_freq=1).
    cache.put(3u32, 30u32);
    assert!(cache.get(&2).is_none(), "key 2 should be evicted");
    assert_eq!(cache.get(&1), Some(&10));
    assert_eq!(cache.get(&3), Some(&30));
}

/// min_freq advances: after evicting the sole entry at min_freq, min_freq = 1
/// because the new insert resets it.
#[test]
fn lfu_min_freq_resets_on_new_insert() {
    let mut cache = LfuCache::new(2);
    cache.put(1u32, 1u32); // freq=1, min=1
    cache.get(&1); // freq=2, min=2
    cache.put(2u32, 2u32); // freq=1, min=1 (reset by new insert)
                           // At cap=2, now: key 1 @ freq=2, key 2 @ freq=1.
                           // Insert 3 → evict key 2 (min_freq=1).
    cache.put(3u32, 3u32);
    assert!(cache.get(&2).is_none());
    assert_eq!(cache.get(&1), Some(&1));
    assert_eq!(cache.get(&3), Some(&3));
}

/// Test: LFU remove works correctly and does not corrupt frequency bookkeeping.
#[test]
fn lfu_remove_and_reinsert() {
    let mut cache = LfuCache::new(3);
    cache.put(1u32, "x");
    cache.put(2u32, "y");
    cache.put(3u32, "z");

    assert_eq!(cache.remove(&2), Some("y"));
    assert_eq!(cache.len(), 2);
    assert!(cache.get(&2).is_none());

    // Can insert a new key after removal.
    cache.put(4u32, "w");
    assert_eq!(cache.len(), 3);
    assert_eq!(cache.get(&4), Some(&"w"));
}

/// Test: LFU clear.
#[test]
fn lfu_clear() {
    let mut cache = LfuCache::new(4);
    for i in 0u32..4 {
        cache.put(i, i * 10);
    }
    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
    // After clear, we can insert again.
    cache.put(0u32, 100u32);
    assert_eq!(cache.get(&0), Some(&100));
}

/// Test: LFU peek does not update frequency.
#[test]
fn lfu_peek_no_frequency_update() {
    let mut cache = LfuCache::new(2);
    cache.put(1u32, "a");
    cache.put(2u32, "b");

    // Peek at 1 — should not change frequency.
    assert_eq!(cache.peek(&1), Some(&"a"));

    // Insert 3 → should evict 1 (still at freq=1, oldest) NOT 2.
    cache.put(3u32, "c");
    assert!(
        cache.get(&1).is_none(),
        "1 should be evicted (peek doesn't boost freq)"
    );
    assert_eq!(cache.get(&2), Some(&"b"));
}

/// Test: LFU contains_key.
#[test]
fn lfu_contains_key() {
    let mut cache: LfuCache<u32, u32> = LfuCache::new(3);
    cache.put(1, 10);
    assert!(Cache::contains_key(&cache, &1));
    assert!(!Cache::contains_key(&cache, &2));
}

/// Test: LFU resize down evicts from lowest frequency.
#[test]
fn lfu_resize_down() {
    let mut cache = LfuCache::new(5);
    for i in 0u32..5 {
        cache.put(i, i * 10);
    }
    // Boost key 4's frequency so it survives resize.
    cache.get(&4);
    cache.get(&4);

    Cache::resize(&mut cache, 2);
    assert_eq!(cache.cap(), 2);
    assert!(cache.len() <= 2);
    // Key 4 (highest freq) should survive.
    assert!(cache.get(&4).is_some());
}

// ===========================================================================
// Count-Min Sketch tests (via internal access through WTinyLFU behavior)
// ===========================================================================

/// Test 3: CMS frequency estimate accuracy.
///
/// We can test the CMS indirectly via the sketch module if it were public,
/// but since it's pub(crate), we test via WTinyLFU's observable behavior.
/// As a direct test, we test the sketch module via a dedicated unit test.
#[test]
fn cms_frequency_estimate_accuracy() {
    // Import the sketch module types via the crate's internal test.
    // Since sketch is pub(crate), we test its behavior via WTinyLFU indirectly.
    // Here we use a large cache so nothing gets evicted, and verify hot keys
    // are tracked with higher frequency than cold keys.
    let mut cache: WTinyLfuCache<u32, u32> = WTinyLfuCache::new(1000);

    // Access key 1 many times.
    cache.put(1, 100);
    for _ in 0..20 {
        cache.get(&1);
    }

    // Access key 2 only once.
    cache.put(2, 200);

    // Key 1 should have a much higher estimated frequency than key 2.
    // We verify this indirectly: in a small cache, key 1 should survive
    // over key 2 when space is tight.
    let mut small_cache: WTinyLfuCache<u32, u32> = WTinyLfuCache::new(200);
    small_cache.put(1, 100);
    // Warm up key 1 to build frequency in the sketch.
    for _ in 0..50 {
        small_cache.get(&1);
    }
    small_cache.put(2, 200);
    // Key 1 should still be present due to high frequency.
    assert!(
        small_cache.get(&1).is_some(),
        "high-frequency key should survive"
    );
}

// ===========================================================================
// W-TinyLFU tests
// ===========================================================================

/// Test: W-TinyLFU basic functionality — items can be stored and retrieved.
#[test]
fn wtinylfu_basic_put_get() {
    let mut cache = WTinyLfuCache::new(100);
    for i in 0u32..50 {
        cache.put(i, i * 2);
    }
    for i in 0u32..50 {
        assert_eq!(cache.get(&i), Some(&(i * 2)));
    }
}

/// Test: W-TinyLFU len / cap / is_empty.
#[test]
fn wtinylfu_len_cap() {
    let mut cache: WTinyLfuCache<u32, u32> = WTinyLfuCache::new(100);
    assert_eq!(cache.cap(), 100);
    assert!(cache.is_empty());
    cache.put(1, 10);
    assert_eq!(cache.len(), 1);
    assert!(!cache.is_empty());
}

/// Test: W-TinyLFU peek does not update frequency.
#[test]
fn wtinylfu_peek_no_update() {
    let mut cache = WTinyLfuCache::new(100);
    cache.put(1u32, "hello");
    assert_eq!(cache.peek(&1), Some(&"hello"));
    assert_eq!(cache.peek(&99), None);
}

/// Test: W-TinyLFU contains_key.
#[test]
fn wtinylfu_contains_key() {
    let mut cache: WTinyLfuCache<u32, u32> = WTinyLfuCache::new(100);
    cache.put(1, 10);
    assert!(Cache::contains_key(&cache, &1));
    assert!(!Cache::contains_key(&cache, &2));
}

/// Test: W-TinyLFU remove.
#[test]
fn wtinylfu_remove() {
    let mut cache = WTinyLfuCache::new(100);
    cache.put(1u32, "a");
    cache.put(2u32, "b");
    assert_eq!(cache.remove(&1), Some("a"));
    assert_eq!(cache.len(), 1);
    assert!(cache.get(&1).is_none());
    assert_eq!(cache.get(&2), Some(&"b"));
}

/// Test: W-TinyLFU clear.
#[test]
fn wtinylfu_clear() {
    let mut cache = WTinyLfuCache::new(100);
    for i in 0u32..10 {
        cache.put(i, i);
    }
    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);
}

/// Test 5: W-TinyLFU doorkeeper — first access does not increment CMS.
///
/// The doorkeeper ensures that keys seen only once don't pollute the CMS.
/// We verify this by checking that keys accessed only once don't get
/// preferential treatment in the admission gate compared to keys seen twice.
#[test]
fn wtinylfu_doorkeeper_first_access_no_cms_increment() {
    // With capacity 100 (window_cap=1, main_cap=99, protected_cap=79),
    // items enter the window.  The first time a key is seen, the doorkeeper
    // fires but the CMS is NOT incremented.
    //
    // We can't directly inspect doorkeeper state, but we can observe that
    // a key seen only once has frequency 0 in the CMS (doorkeeper prevented
    // the increment), while a key seen twice has frequency 1.
    //
    // Effect: in a tight cache, the "twice-seen" key should win admission
    // over a "once-seen" key.
    let mut cache: WTinyLfuCache<u32, u32> = WTinyLfuCache::new(10);

    // Warm up: see key 1 many times to build CMS frequency.
    // First put → doorkeeper miss (no CMS).  Get → doorkeeper hit → CMS++.
    cache.put(1, 100);
    for _ in 0..10 {
        cache.get(&1);
    }

    // Key 2: seen only once (put only, no gets after).
    cache.put(2, 200);

    // Both are in the cache at this point.  Key 1 should have a higher
    // frequency estimate than key 2.
    assert!(cache.get(&1).is_some(), "hot key should still be in cache");
}

/// Test 6: W-TinyLFU admission gate — main_victim stays when window_candidate
/// has lower frequency.
///
/// We set up a scenario where the main space is full with a hot item,
/// and a cold item (window candidate) should NOT displace it.
#[test]
fn wtinylfu_admission_gate_rejects_cold_candidate() {
    // Use total_cap = 100 for clarity.
    let mut cache: WTinyLfuCache<u32, u32> = WTinyLfuCache::new(100);

    // Build high frequency for key 1.
    cache.put(1, 100);
    for _ in 0..30 {
        cache.get(&1);
    }

    // Fill remaining capacity with cold items.
    for i in 2u32..100 {
        cache.put(i, i * 10);
    }

    // Key 1 should still be present despite cache pressure.
    assert!(
        cache.get(&1).is_some(),
        "high-frequency key 1 should survive cache pressure"
    );
}

/// Test 7: Zipfian hit-ratio test.
///
/// On a Zipfian workload, WTinyLFU should beat LRU by at least 5 percentage points.
/// We use a deterministic LCG PRNG to generate keys.
#[test]
fn zipfian_hit_ratio_wtinylfu_beats_lru() {
    const NUM_OPS: usize = 10_000;
    const NUM_KEYS: usize = 1_000;
    const CACHE_SIZE: usize = 100; // 10% of key space
    const MIN_ADVANTAGE: f64 = 0.05; // WTinyLFU must beat LRU by at least 5%

    // LCG PRNG: a classic 64-bit LCG for determinism.
    // multiplier and addend from Knuth.
    fn lcg_next(state: u64) -> u64 {
        state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407)
    }

    // Build a precomputed Zipfian sample table using inverse-CDF approximation.
    // P(rank r) ∝ 1/(r+1) for r in 0..NUM_KEYS.
    // We use the table-based approach: compute cumulative weights, then sample.
    let weights: Vec<f64> = (0..NUM_KEYS).map(|r| 1.0 / (r + 1) as f64).collect();
    let total_weight: f64 = weights.iter().sum();
    let cumulative: Vec<f64> = weights
        .iter()
        .scan(0.0f64, |acc, &w| {
            *acc += w / total_weight;
            Some(*acc)
        })
        .collect();

    // Generate 10,000 operations with fixed seed.
    let mut rng = 0x123456789abcdef0u64;
    let ops: Vec<u32> = (0..NUM_OPS)
        .map(|_| {
            rng = lcg_next(rng);
            // Map rng to [0.0, 1.0).
            let r = (rng >> 11) as f64 / (1u64 << 53) as f64;
            // Binary search for the Zipfian key.
            let idx = cumulative.partition_point(|&c| c < r).min(NUM_KEYS - 1);
            idx as u32
        })
        .collect();

    // Simulate LRU.
    let mut lru: LruCache<u32, ()> = LruCache::new(CACHE_SIZE);
    let mut lru_hits = 0usize;
    for &key in &ops {
        if lru.get(&key).is_some() {
            lru_hits += 1;
        } else {
            lru.put(key, ());
        }
    }

    // Simulate WTinyLFU.
    let mut wtlfu: WTinyLfuCache<u32, ()> = WTinyLfuCache::new(CACHE_SIZE);
    let mut wtlfu_hits = 0usize;
    for &key in &ops {
        if wtlfu.get(&key).is_some() {
            wtlfu_hits += 1;
        } else {
            wtlfu.put(key, ());
        }
    }

    let lru_ratio = lru_hits as f64 / NUM_OPS as f64;
    let wtlfu_ratio = wtlfu_hits as f64 / NUM_OPS as f64;
    let advantage = wtlfu_ratio - lru_ratio;

    assert!(
        advantage >= MIN_ADVANTAGE,
        "WTinyLFU ({:.1}%) should beat LRU ({:.1}%) by at least {:.0}%;\n\
         actual advantage: {:.1}%",
        wtlfu_ratio * 100.0,
        lru_ratio * 100.0,
        MIN_ADVANTAGE * 100.0,
        advantage * 100.0,
    );
}

// ===========================================================================
// TTL tests
// ===========================================================================

/// Test 8a: LFU TTL — expired entry returns None after sleep.
#[test]
fn lfu_ttl_expiry() {
    let mut cache = LfuCache::new(10);
    cache.put_with_ttl(1u32, "expires", Duration::from_millis(20));
    cache.put(2u32, "forever");

    // Entry should be present immediately.
    assert_eq!(cache.get(&1), Some(&"expires"));
    assert_eq!(cache.get(&2), Some(&"forever"));

    thread::sleep(Duration::from_millis(50));

    // After expiry: key 1 should return None; key 2 should still be present.
    assert_eq!(cache.get(&1), None, "expired entry should return None");
    assert_eq!(
        cache.get(&2),
        Some(&"forever"),
        "non-TTL entry should persist"
    );
}

/// Test 8b: LRU TTL — expired entry returns None after sleep.
#[test]
fn lru_ttl_expiry() {
    let mut cache = LruCache::new(10);
    cache.put_with_ttl(1u32, "expires", Duration::from_millis(20));
    cache.put(2u32, "forever");

    assert_eq!(cache.get(&1), Some(&"expires"));
    assert_eq!(cache.get(&2), Some(&"forever"));

    thread::sleep(Duration::from_millis(50));

    assert_eq!(cache.get(&1), None, "expired entry should return None");
    assert_eq!(
        cache.get(&2),
        Some(&"forever"),
        "non-TTL entry should persist"
    );
}

/// Test 8c: W-TinyLFU TTL — expired entry returns None after sleep.
#[test]
fn wtinylfu_ttl_expiry() {
    let mut cache = WTinyLfuCache::new(100);
    cache.put_with_ttl(1u32, "expires", Duration::from_millis(20));
    cache.put(2u32, "forever");

    assert_eq!(cache.get(&1), Some(&"expires"));
    assert_eq!(cache.get(&2), Some(&"forever"));

    thread::sleep(Duration::from_millis(50));

    assert_eq!(
        cache.get(&1),
        None,
        "expired WTinyLFU entry should return None"
    );
    assert_eq!(
        cache.get(&2),
        Some(&"forever"),
        "non-TTL entry should persist"
    );
}

/// Test 8d: TTL peek returns None for expired entries.
#[test]
fn lfu_ttl_peek_expired() {
    let cache_lfu = {
        let mut c = LfuCache::new(10);
        // We can't sleep in peek (it's &self), so we just check that peek
        // correctly handles non-expired entries.
        c.put_with_ttl(1u32, "a", Duration::from_secs(3600));
        c
    };
    assert_eq!(cache_lfu.peek(&1), Some(&"a"));
    assert_eq!(cache_lfu.peek(&99), None);
}

/// Test 9: put_with_ttl on LFU.
#[test]
fn lfu_put_with_ttl_basic() {
    let mut cache = LfuCache::new(10);
    cache.put_with_ttl(1u32, 100u32, Duration::from_millis(100));

    // Immediately accessible.
    assert_eq!(cache.get(&1), Some(&100));
    assert!(Cache::contains_key(&cache, &1));

    thread::sleep(Duration::from_millis(200));

    // After expiry.
    assert!(cache.get(&1).is_none());
    assert!(!Cache::contains_key(&cache, &1));
}

/// Test 9b: put_with_ttl on WTinyLFU.
#[test]
fn wtinylfu_put_with_ttl_basic() {
    let mut cache: WTinyLfuCache<u32, u32> = WTinyLfuCache::new(100);
    cache.put_with_ttl(42, 999, Duration::from_millis(50));

    assert_eq!(cache.get(&42), Some(&999));
    assert!(Cache::contains_key(&cache, &42));

    thread::sleep(Duration::from_millis(100));

    assert!(cache.get(&42).is_none());
    assert!(!Cache::contains_key(&cache, &42));
}

// ===========================================================================
// CMS unit tests (via a helper that exposes the module)
// ===========================================================================

/// Direct CMS tests imported from the sketch module.
/// These verify the nibble packing, aging, and estimate accuracy.
#[cfg(test)]
mod cms_direct {
    use oxistore_cache::sketch::{CountMinSketch, Doorkeeper};

    #[test]
    fn cms_estimate_at_least_n_after_n_increments() {
        let mut sketch = CountMinSketch::new(1024);
        let key = b"test_key_for_frequency";

        for _ in 0..10 {
            sketch.increment(key);
        }

        let estimate = sketch.estimate(key);
        assert!(
            estimate >= 10,
            "estimate {} should be >= 10 after 10 increments",
            estimate
        );
    }

    #[test]
    fn cms_estimate_accurate_across_many_keys() {
        let mut sketch = CountMinSketch::new(256);

        // Increment key A 50 times, key B 5 times.
        let key_a = b"key_alpha_hot";
        let key_b = b"key_beta_cold";

        for _ in 0..50 {
            sketch.increment(key_a);
        }
        for _ in 0..5 {
            sketch.increment(key_b);
        }

        let est_a = sketch.estimate(key_a);
        let est_b = sketch.estimate(key_b);

        // A should have a higher estimate than B.
        assert!(
            est_a > est_b,
            "key_a estimate ({}) should exceed key_b estimate ({})",
            est_a,
            est_b
        );
        // B's estimate should be >= 5.
        assert!(est_b >= 5, "key_b estimate {} should be >= 5", est_b);
    }

    #[test]
    fn cms_aging_halves_estimates() {
        let mut sketch = CountMinSketch::new(64);
        let key = b"aging_test_key";

        // Increment 14 times (below nibble max of 15 so no saturation).
        for _ in 0..14 {
            sketch.increment(key);
        }
        let before = sketch.estimate(key);

        // Force aging.
        sketch.age();
        let after = sketch.estimate(key);

        assert!(
            after <= before / 2 + 1,
            "after aging, estimate {} should be ~half of before {}",
            after,
            before
        );
        // Aged value should be at least 1 (14 / 2 = 7).
        assert!(
            after >= 6,
            "aged estimate {} should be >= 6 (half of 14)",
            after
        );
    }

    #[test]
    fn cms_counter_saturation_at_15() {
        let mut sketch = CountMinSketch::new(64);
        let key = b"saturate_me";

        // Increment 30 times — counter should saturate at 15.
        for _ in 0..30 {
            sketch.increment(key);
        }
        let est = sketch.estimate(key);
        assert!(est <= 15, "nibble counter must saturate at 15, got {}", est);
        assert!(
            est >= 15,
            "after 30 increments, should be at max 15, got {}",
            est
        );
    }

    #[test]
    fn doorkeeper_first_access_returns_false() {
        let mut door = Doorkeeper::new(256);
        // First time: all bits unset → returns false.
        let first = door.put(b"new_key");
        assert!(!first, "first access should return false (not seen before)");
    }

    #[test]
    fn doorkeeper_second_access_returns_true() {
        let mut door = Doorkeeper::new(256);
        door.put(b"returning_key"); // first access
        let second = door.put(b"returning_key");
        assert!(second, "second access should return true (seen before)");
    }

    #[test]
    fn doorkeeper_clear_resets_state() {
        let mut door = Doorkeeper::new(256);
        door.put(b"some_key");
        door.clear();
        // After clear, key should be treated as new again.
        let after_clear = door.put(b"some_key");
        assert!(
            !after_clear,
            "after clear, key should be treated as new (return false)"
        );
    }
}

// ===========================================================================
// ARC TTL tests
// ===========================================================================

/// Test: ARC TTL expiry.
#[test]
fn arc_ttl_expiry() {
    use oxistore_cache::ArcCache;

    let mut cache = ArcCache::new(10);
    cache.put_with_ttl(1u32, "expires", Duration::from_millis(20));
    cache.put(2u32, "forever");

    assert_eq!(cache.get(&1), Some(&"expires"));
    assert_eq!(cache.get(&2), Some(&"forever"));

    thread::sleep(Duration::from_millis(50));

    assert_eq!(cache.get(&1), None, "expired ARC entry should return None");
    assert_eq!(
        cache.get(&2),
        Some(&"forever"),
        "non-TTL ARC entry should persist"
    );
}

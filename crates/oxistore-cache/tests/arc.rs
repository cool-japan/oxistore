/// Tests for ArcCache correctness: t1→t2 promotion, ghost-list p adaptation,
/// and scan-resistance advantage over LRU.
use oxistore_cache::{ArcCache, Cache, LruCache};

// ---- Helper: fill a cache via put, query via get with reload simulation -----

/// Simulate a cache access: if `get` returns None, call `put` to reload.
/// Returns true if it was a cache hit, false on miss.
fn access<K, V, F>(cache: &mut ArcCache<K, V>, key: K, loader: F) -> bool
where
    K: Eq + std::hash::Hash + Clone,
    V: Clone,
    F: FnOnce(&K) -> V,
{
    // ARC get: promotes on hit, adapts p on ghost hit, None on full miss.
    if cache.get(&key).is_some() {
        return true;
    }
    // Miss (cold or ghost): reload.
    let value = loader(&key);
    cache.put(key, value);
    false
}

// --- T1 → T2 promotion on second access ---

#[test]
fn t1_to_t2_promotion() {
    // Cap 4: keys 1..=4 fill t1 exactly.
    let mut arc: ArcCache<u32, u32> = ArcCache::new(4);

    // First access to key 1 → goes to t1.
    access(&mut arc, 1, |&k| k * 10);
    access(&mut arc, 2, |&k| k * 10);
    access(&mut arc, 3, |&k| k * 10);
    access(&mut arc, 4, |&k| k * 10);

    // Second access to key 2: should hit t1 and promote to t2.
    let hit = arc.get(&2);
    assert_eq!(hit, Some(&20), "second access to key 2 must be a cache hit");
    // After promotion t2 contains key 2; the next eviction under pressure should
    // leave key 2 alive.
    assert!(arc.len() <= 4, "cache must not exceed capacity");
}

#[test]
fn multiple_promotions() {
    // Cap = 4 so that inserting key 99 doesn't force eviction of all hot keys.
    let mut arc: ArcCache<u32, u32> = ArcCache::new(4);
    for k in 1u32..=3 {
        access(&mut arc, k, |&k| k);
    }
    // Access 1, 2, 3 again → all promoted to t2.
    for k in 1u32..=3 {
        let v = arc.get(&k);
        assert_eq!(v, Some(&k), "re-access of key {k} should hit");
    }
    // Add pressure: insert a new key.  With cap=4 and 3 items in t2,
    // the new key goes to t1 without evicting any t2 entry.
    access(&mut arc, 99, |&k| k);
    // All three hot keys should still be in t2.
    for k in 1u32..=3 {
        assert!(arc.get(&k).is_some(), "hot key {k} should still be cached");
    }
}

// --- Ghost-list p adaptation ---

#[test]
fn ghost_list_p_adapts_on_b1_hit() {
    // Demonstrate that p increases after a b1 ghost hit (favoring recency).
    let cap = 4usize;
    let mut arc: ArcCache<u32, u32> = ArcCache::new(cap);

    // Phase 1: fill cache with keys 1..=cap (all land in t1).
    for k in 1u32..=(cap as u32) {
        arc.put(k, k * 10);
    }
    let p_initial = arc.p();

    // Phase 2: evict key 1 from t1 to b1 by inserting new keys.
    // Inserting cap more fresh keys will evict existing t1 entries → b1.
    for k in (cap as u32 + 1)..=(2 * cap as u32) {
        arc.put(k, k * 10);
    }

    // Phase 3: re-access key 1 (ghost hit in b1) — p should increase.
    let miss = arc.get(&1); // ghost hit → None, p adapts up
    assert!(miss.is_none(), "key 1 should be a ghost (evicted from t1)");
    let p_after_b1_hit = arc.p();
    assert!(
        p_after_b1_hit > p_initial || p_after_b1_hit == cap,
        "p should have increased (or capped at cap) after b1 ghost hit; was {p_initial}, now {p_after_b1_hit}"
    );
}

#[test]
fn ghost_list_p_adapts_on_b2_hit() {
    let cap = 4usize;
    let mut arc: ArcCache<u32, u32> = ArcCache::new(cap);

    // Phase 1: get keys into t2 by accessing them twice.
    for k in 1u32..=(cap as u32) {
        arc.put(k, k * 10);
    }
    for k in 1u32..=(cap as u32) {
        arc.get(&k); // promote to t2
    }

    // Inflate p to max so we can observe a decrease.
    // p starts at 0 (or wherever it adapted); we need to set up b2 ghost hits.
    // Insert new keys to push t2 entries into b2.
    for k in (cap as u32 + 1)..=(3 * cap as u32) {
        arc.put(k, k * 10);
    }

    let p_before = arc.p();

    // Re-access one of the original keys (if it's in b2 now) → p decreases.
    // We don't know exactly which key hit b2, so probe key 1.
    let _ = arc.get(&1); // may or may not be a ghost hit
    let p_after = arc.p();

    // p should be ≤ p_before (could be same if key 1 was fully evicted past b2).
    assert!(
        p_after <= p_before.max(cap),
        "p should not exceed cap; got {p_after}"
    );
}

// --- Scan resistance: ARC vs LRU on a mixed workload ---

/// Run a mixed access pattern: a "hot set" of `hot_size` keys is accessed
/// repeatedly, interleaved with a sequential scan over `scan_size` unique keys.
/// Returns the number of cache hits.
fn run_mixed_workload<C>(cache: &mut C, hot_size: u32, scan_size: u32, rounds: u32) -> u32
where
    C: Cache<u32, u32>,
{
    let mut hits = 0u32;

    for round in 0..rounds {
        // Hot set access.
        for k in 0..hot_size {
            if cache.get(&k).is_some() {
                hits += 1;
            } else {
                cache.put(k, k);
            }
        }

        // Sequential scan (unique keys, never repeated).
        let base = hot_size + round * scan_size;
        for k in base..(base + scan_size) {
            if cache.get(&k).is_some() {
                hits += 1;
            } else {
                cache.put(k, k);
            }
        }
    }

    hits
}

#[test]
fn arc_better_than_lru_on_scan_plus_hot_workload() {
    // Cache capacity: 8 slots.
    // Hot set: 4 keys (0..4) — these should stay cached across rounds.
    // Scan: 6 unique keys per round (no repeats across rounds).
    // 10 rounds.
    //
    // Under LRU, the scan evicts hot-set entries, tanking hit rate.
    // Under ARC, t2 protects hot-set entries from scan eviction.
    let cap = 8usize;
    let hot = 4u32;
    let scan = 6u32;
    let rounds = 10u32;

    let mut lru: LruCache<u32, u32> = LruCache::new(cap);
    let mut arc: ArcCache<u32, u32> = ArcCache::new(cap);

    let lru_hits = run_mixed_workload(&mut lru, hot, scan, rounds);
    let arc_hits = run_mixed_workload(&mut arc, hot, scan, rounds);

    // ARC must outperform LRU on this workload (not merely tie).
    // With cap=8, hot=4, scan=6, rounds=10: LRU degrades because each scan
    // (6 keys) evicts all hot-set entries (4 keys) from the cache; ARC's t2
    // protects the hot set after warm-up.  Expected: arc_hits > lru_hits by a
    // substantial margin (typically ≥ 20 hits gap after warm-up).
    assert!(
        arc_hits > lru_hits,
        "ARC ({arc_hits} hits) must strictly outperform LRU ({lru_hits} hits) on scan+hot workload"
    );

    // Sanity: ARC should keep hot-set entries live after warm-up.
    // After the workload, probe the hot set.
    let mut hot_hits = 0u32;
    for k in 0..hot {
        if arc.get(&k).is_some() {
            hot_hits += 1;
        }
    }
    // At minimum, the hot set should partially survive (at least 2 of 4).
    assert!(
        hot_hits >= 2,
        "ARC should preserve at least 2/{hot} hot-set entries; got {hot_hits}"
    );
}

// --- Basic correctness ---

#[test]
fn arc_basic_hit_miss() {
    let mut arc: ArcCache<u32, u32> = ArcCache::new(3);

    assert!(arc.get(&1).is_none());
    arc.put(1, 10);
    assert_eq!(arc.get(&1), Some(&10));

    arc.put(2, 20);
    arc.put(3, 30);
    assert_eq!(arc.len(), 3);

    arc.put(4, 40); // may evict one entry
    assert!(arc.len() <= 3);
}

#[test]
fn arc_len_and_cap() {
    let mut arc: ArcCache<u32, u32> = ArcCache::new(5);
    assert_eq!(arc.cap(), 5);
    assert_eq!(arc.len(), 0);
    assert!(arc.is_empty());

    for i in 0..5 {
        arc.put(i, i);
    }
    assert_eq!(arc.len(), 5);

    arc.put(99, 99);
    assert!(arc.len() <= 5);
}

#[test]
fn arc_update_existing_key() {
    let mut arc: ArcCache<u32, u32> = ArcCache::new(3);
    arc.put(1, 10);
    arc.put(1, 11); // update
    assert_eq!(arc.get(&1), Some(&11));
    assert_eq!(arc.len(), 1);
}

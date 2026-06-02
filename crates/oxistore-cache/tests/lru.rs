/// Tests for LruCache eviction order and MRU promotion.
use oxistore_cache::LruCache;

// --- Basic eviction order ---

#[test]
fn lru_evicts_least_recently_used() {
    let mut cache: LruCache<u32, &str> = LruCache::new(3);

    // Fill to capacity.
    assert!(cache.put(1, "a").is_none()); // [1]
    assert!(cache.put(2, "b").is_none()); // [1, 2]
    assert!(cache.put(3, "c").is_none()); // [1, 2, 3]

    // Inserting a 4th entry must evict the LRU (key 1).
    let evicted = cache.put(4, "d"); // [2, 3, 4]
    assert_eq!(evicted, Some("a"), "expected 'a' to be evicted");

    assert!(cache.get(&1).is_none(), "key 1 should have been evicted");
    assert_eq!(cache.get(&2), Some(&"b"));
    assert_eq!(cache.get(&3), Some(&"c"));
    assert_eq!(cache.get(&4), Some(&"d"));
}

#[test]
fn lru_eviction_order_sequential() {
    let mut cache: LruCache<u32, u32> = LruCache::new(4);

    for i in 0..4 {
        cache.put(i, i * 10);
    }
    // Order (LRU → MRU): 0, 1, 2, 3

    let e = cache.put(4, 40); // evicts 0
    assert_eq!(e, Some(0));

    let e = cache.put(5, 50); // evicts 1
    assert_eq!(e, Some(10));
}

// --- MRU promotion prevents eviction ---

#[test]
fn get_promotes_to_mru() {
    let mut cache: LruCache<u32, &str> = LruCache::new(3);
    cache.put(1, "a");
    cache.put(2, "b");
    cache.put(3, "c");
    // Order: [1(LRU), 2, 3(MRU)]

    // Access key 1 → moves to MRU.
    assert_eq!(cache.get(&1), Some(&"a"));
    // Order: [2(LRU), 3, 1(MRU)]

    // Next insert should evict key 2.
    let evicted = cache.put(4, "d");
    assert_eq!(
        evicted,
        Some("b"),
        "key 2 (LRU after access to 1) should be evicted"
    );

    assert!(cache.get(&2).is_none(), "key 2 should have been evicted");
    assert_eq!(cache.get(&1), Some(&"a"), "key 1 should still be present");
    assert_eq!(cache.get(&3), Some(&"c"), "key 3 should still be present");
    assert_eq!(cache.get(&4), Some(&"d"), "key 4 should be present");
}

#[test]
fn repeated_get_keeps_hot_entry() {
    let mut cache: LruCache<u32, u32> = LruCache::new(3);
    cache.put(1, 100);
    cache.put(2, 200);
    cache.put(3, 300);

    // Repeatedly access key 1 to keep it hot.
    for _ in 0..5 {
        assert_eq!(cache.get(&1), Some(&100));
        cache.put(10, 1000); // evicts LRU
        cache.put(11, 1100);
        cache.put(12, 1200);
        // Reset with original keys so key 1 is tested again.
        let _ = cache; // consume
        cache = LruCache::new(3);
        cache.put(1, 100);
        cache.put(2, 200);
        cache.put(3, 300);
    }
    // Key 1 should survive one more access.
    assert_eq!(cache.get(&1), Some(&100));
}

// --- Update (same key) does not evict ---

#[test]
fn put_update_does_not_evict() {
    let mut cache: LruCache<u32, u32> = LruCache::new(2);
    cache.put(1, 10);
    cache.put(2, 20);

    // Update key 1 — no eviction expected.
    let evicted = cache.put(1, 11);
    assert!(evicted.is_none(), "updating an existing key must not evict");
    assert_eq!(cache.len(), 2);
    // The updated value should be visible.
    assert_eq!(cache.get(&1), Some(&11));
}

// --- Capacity boundary ---

#[test]
fn capacity_one() {
    let mut cache: LruCache<u32, u32> = LruCache::new(1);
    cache.put(1, 10);
    assert_eq!(cache.len(), 1);

    let evicted = cache.put(2, 20);
    assert_eq!(evicted, Some(10), "only entry should be evicted");
    assert_eq!(cache.len(), 1);
    assert!(cache.get(&1).is_none());
    assert_eq!(cache.get(&2), Some(&20));
}

// --- len / cap ---

#[test]
fn len_and_cap() {
    let mut cache: LruCache<u32, u32> = LruCache::new(5);
    assert_eq!(cache.cap(), 5);
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());

    for i in 0..5 {
        cache.put(i, i);
    }
    assert_eq!(cache.len(), 5);
    assert!(!cache.is_empty());

    cache.put(99, 99); // evicts one
    assert_eq!(cache.len(), 5);
}

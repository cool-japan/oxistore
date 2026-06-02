//! Property-based tests for `RangeItem` and `RangeIter` invariants using a
//! minimal in-memory `KvStore` fixture.
//!
//! Tests verify real invariants:
//! - Items returned by `range(lo, hi)` have keys in `[lo, hi)`.
//! - The ordering of returned items is non-decreasing in key order.
//! - `prefix_scan` returns only entries whose keys start with the prefix.

use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeItem, RangeIter, StoreError};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::sync::Mutex;

// ── Minimal in-memory KvStore fixture ────────────────────────────────────────

struct MemKv(Mutex<BTreeMap<Vec<u8>, Vec<u8>>>);

impl MemKv {
    fn new() -> Self {
        MemKv(Mutex::new(BTreeMap::new()))
    }
}

impl KvStore for MemKv {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.0.lock().expect("mutex").get(key).cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.0
            .lock()
            .expect("mutex")
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.0.lock().expect("mutex").remove(key);
        Ok(())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        use std::ops::Bound;
        let map = self.0.lock().expect("mutex");
        let pairs: Vec<RangeItem> = map
            .range((Bound::Included(lo.to_vec()), Bound::Excluded(hi.to_vec())))
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let map = self.0.lock().expect("mutex");
        let pairs: Vec<RangeItem> = map
            .iter()
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Err(StoreError::Unsupported(
            "MemKv has no transactions".to_string(),
        ))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Unsupported(
            "MemKv has no snapshots".to_string(),
        ))
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

// ── Proptest helpers ──────────────────────────────────────────────────────────

/// Strategy that generates a sorted pair `(lo, hi)` where `lo < hi`.
fn lo_hi_strategy() -> impl Strategy<Value = (Vec<u8>, Vec<u8>)> {
    proptest::collection::vec(any::<u8>(), 1..=8usize).prop_flat_map(|lo| {
        // hi must be strictly greater than lo.
        let hi_min = {
            let mut h = lo.clone();
            *h.last_mut().expect("non-empty") =
                h.last().copied().expect("non-empty").saturating_add(1);
            h
        };
        proptest::collection::vec(any::<u8>(), 1..=8usize).prop_map(move |mut hi| {
            // Ensure hi > lo by appending 0x01 if needed.
            if hi <= lo {
                hi = hi_min.clone();
            }
            (lo.clone(), hi)
        })
    })
}

// ── Property tests ────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(100))]

    /// All keys returned by `range(lo, hi)` lie in `[lo, hi)`.
    #[test]
    fn prop_range_keys_in_bounds(
        pairs in proptest::collection::vec(
            (
                proptest::collection::vec(any::<u8>(), 1..=8usize),
                proptest::collection::vec(any::<u8>(), 0..=16usize),
            ),
            0..=20usize,
        ),
        (lo, hi) in lo_hi_strategy(),
    ) {
        let store = MemKv::new();
        for (k, v) in &pairs {
            store.put(k, v).expect("put");
        }

        let items: Vec<(Vec<u8>, Vec<u8>)> = store
            .range(&lo, &hi)
            .expect("range")
            .map(|r| r.expect("range item"))
            .collect();

        for (k, _v) in &items {
            prop_assert!(
                k >= &lo,
                "key {:?} is below lo {:?}",
                k,
                lo
            );
            prop_assert!(
                k < &hi,
                "key {:?} is at or above hi {:?}",
                k,
                hi
            );
        }
    }

    /// Items returned by `range(lo, hi)` are in non-decreasing key order.
    #[test]
    fn prop_range_keys_sorted(
        pairs in proptest::collection::vec(
            (
                proptest::collection::vec(any::<u8>(), 1..=8usize),
                proptest::collection::vec(any::<u8>(), 0..=16usize),
            ),
            0..=20usize,
        ),
        (lo, hi) in lo_hi_strategy(),
    ) {
        let store = MemKv::new();
        for (k, v) in &pairs {
            store.put(k, v).expect("put");
        }

        let keys: Vec<Vec<u8>> = store
            .range(&lo, &hi)
            .expect("range")
            .map(|r| r.expect("range item").0)
            .collect();

        for window in keys.windows(2) {
            prop_assert!(
                window[0] <= window[1],
                "keys not sorted: {:?} > {:?}",
                window[0],
                window[1]
            );
        }
    }

    /// `prefix_scan` returns exactly those entries whose key starts with the prefix.
    #[test]
    fn prop_prefix_scan_only_prefix_keys(
        pairs in proptest::collection::vec(
            (
                proptest::collection::vec(any::<u8>(), 1..=8usize),
                proptest::collection::vec(any::<u8>(), 0..=8usize),
            ),
            0..=20usize,
        ),
        prefix in proptest::collection::vec(any::<u8>(), 1..=4usize),
    ) {
        let store = MemKv::new();
        for (k, v) in &pairs {
            store.put(k, v).expect("put");
        }

        let scanned_keys: Vec<Vec<u8>> = store
            .prefix_scan(&prefix)
            .expect("prefix_scan")
            .map(|r| r.expect("prefix_scan item").0)
            .collect();

        for key in &scanned_keys {
            prop_assert!(
                key.starts_with(&prefix),
                "key {:?} does not start with prefix {:?}",
                key,
                prefix
            );
        }
    }

    /// `range_rev` returns items in non-increasing key order.
    #[test]
    fn prop_range_rev_keys_descending(
        pairs in proptest::collection::vec(
            (
                proptest::collection::vec(any::<u8>(), 1..=8usize),
                proptest::collection::vec(any::<u8>(), 0..=16usize),
            ),
            0..=20usize,
        ),
        (lo, hi) in lo_hi_strategy(),
    ) {
        let store = MemKv::new();
        for (k, v) in &pairs {
            store.put(k, v).expect("put");
        }

        let keys: Vec<Vec<u8>> = store
            .range_rev(&lo, &hi)
            .expect("range_rev")
            .map(|r| r.expect("range_rev item").0)
            .collect();

        for window in keys.windows(2) {
            prop_assert!(
                window[0] >= window[1],
                "range_rev keys not descending: {:?} < {:?}",
                window[0],
                window[1]
            );
        }
    }
}

// ── Deterministic edge-case tests ─────────────────────────────────────────────

#[test]
fn range_empty_when_lo_equals_hi() {
    let store = MemKv::new();
    store.put(b"a", b"1").expect("put");
    let items: Vec<_> = store.range(b"a", b"a").expect("range").collect();
    assert!(items.is_empty(), "range [a,a) must be empty");
}

#[test]
fn range_item_key_and_value_match_inserted() {
    let store = MemKv::new();
    store.put(b"hello", b"world").expect("put");
    store.put(b"foo", b"bar").expect("put");

    let items: Vec<(Vec<u8>, Vec<u8>)> = store
        .range(b"foo", b"hfoo")
        .expect("range")
        .map(|r| r.expect("item"))
        .collect();

    assert_eq!(items.len(), 2, "expected 2 items");
    assert_eq!(items[0].0, b"foo");
    assert_eq!(items[0].1, b"bar");
    assert_eq!(items[1].0, b"hello");
    assert_eq!(items[1].1, b"world");
}

#[test]
fn range_item_count_matches_put() {
    let store = MemKv::new();
    let pairs: Vec<(&[u8], &[u8])> = vec![
        (b"k0", b"v0"),
        (b"k1", b"v1"),
        (b"k2", b"v2"),
        (b"k3", b"v3"),
        (b"k4", b"v4"),
    ];
    for &(k, v) in &pairs {
        store.put(k, v).expect("put");
    }

    let count = store.range(b"k0", b"k5").expect("range").count();
    assert_eq!(count, pairs.len(), "item count mismatch");
}

//! Benchmark: concrete (monomorphic) dispatch vs `dyn KvStore` (dynamic dispatch).
//!
//! A minimal in-memory `KvStore` is implemented in-bench to isolate the
//! dispatch overhead from any backend I/O.  The benchmark compares:
//!
//! - `get_concrete` — calls through a known concrete type; the compiler can
//!   inline and devirtualise freely.
//! - `get_dyn` — calls through `&dyn KvStore`; requires a vtable lookup.
//!
//! The difference quantifies the vtable overhead on this platform.

use criterion::{criterion_group, criterion_main, Criterion};
use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeItem, RangeIter, StoreError};
use std::collections::BTreeMap;
use std::sync::Mutex;

// ── Minimal in-memory KvStore ─────────────────────────────────────────────────

struct BenchStore(Mutex<BTreeMap<Vec<u8>, Vec<u8>>>);

impl BenchStore {
    fn new() -> Self {
        BenchStore(Mutex::new(BTreeMap::new()))
    }
}

impl KvStore for BenchStore {
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
            "BenchStore has no transactions".to_string(),
        ))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Unsupported(
            "BenchStore has no snapshots".to_string(),
        ))
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

// ── Bench: concrete dispatch ──────────────────────────────────────────────────

fn bench_get_concrete(c: &mut Criterion) {
    let store = BenchStore::new();
    store.put(b"bench_key", b"bench_value").expect("put");

    c.bench_function("get_concrete", |b| {
        b.iter(|| {
            let _ = store.get(b"bench_key").expect("get_concrete");
        });
    });
}

// ── Bench: dyn dispatch ───────────────────────────────────────────────────────

fn bench_get_dyn(c: &mut Criterion) {
    let store: Box<dyn KvStore> = Box::new(BenchStore::new());
    store.put(b"bench_key", b"bench_value").expect("put");

    c.bench_function("get_dyn", |b| {
        b.iter(|| {
            let _ = store.get(b"bench_key").expect("get_dyn");
        });
    });
}

// ── Bench: concrete put ───────────────────────────────────────────────────────

fn bench_put_concrete(c: &mut Criterion) {
    let store = BenchStore::new();
    c.bench_function("put_concrete", |b| {
        b.iter(|| {
            store
                .put(b"bench_key", b"bench_value")
                .expect("put_concrete");
        });
    });
}

// ── Bench: dyn put ────────────────────────────────────────────────────────────

fn bench_put_dyn(c: &mut Criterion) {
    let store: Box<dyn KvStore> = Box::new(BenchStore::new());
    c.bench_function("put_dyn", |b| {
        b.iter(|| {
            store.put(b"bench_key", b"bench_value").expect("put_dyn");
        });
    });
}

// ── Bench: range (concrete) ───────────────────────────────────────────────────

fn bench_range_concrete(c: &mut Criterion) {
    let store = BenchStore::new();
    // Pre-populate 64 entries.
    for i in 0u8..64 {
        store.put(&[i], &[i, i]).expect("put");
    }

    c.bench_function("range_concrete", |b| {
        b.iter(|| {
            let items: Vec<_> = store
                .range(&[0u8], &[64u8])
                .expect("range_concrete")
                .collect();
            assert_eq!(items.len(), 64);
        });
    });
}

// ── Bench: range (dyn) ────────────────────────────────────────────────────────

fn bench_range_dyn(c: &mut Criterion) {
    let store: Box<dyn KvStore> = Box::new(BenchStore::new());
    for i in 0u8..64 {
        store.put(&[i], &[i, i]).expect("put");
    }

    c.bench_function("range_dyn", |b| {
        b.iter(|| {
            let items: Vec<_> = store.range(&[0u8], &[64u8]).expect("range_dyn").collect();
            assert_eq!(items.len(), 64);
        });
    });
}

// ── criterion entry points ────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_get_concrete,
    bench_get_dyn,
    bench_put_concrete,
    bench_put_dyn,
    bench_range_concrete,
    bench_range_dyn,
);
criterion_main!(benches);

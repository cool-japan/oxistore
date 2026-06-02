//! Criterion benchmarks for the OxiStore encrypt decorator.
//!
//! Measures `put` (encrypt + store) and `get` (load + decrypt) throughput for
//! the default XChaCha20-Poly1305 cipher across a range of payload sizes.

use std::collections::HashMap;
use std::sync::Mutex;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
use oxistore_encrypt::{derive_cell_id, EncryptedKv, StaticKey};

// ── Minimal in-memory KvStore for benchmarks ─────────────────────────────────

#[derive(Default)]
struct MemStore(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

impl KvStore for MemStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .0
            .lock()
            .expect("bench MemStore lock poisoned")
            .get(key)
            .cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.0
            .lock()
            .expect("bench MemStore lock poisoned")
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.0
            .lock()
            .expect("bench MemStore lock poisoned")
            .remove(key);
        Ok(())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let guard = self.0.lock().expect("bench MemStore lock poisoned");
        let lo = lo.to_vec();
        let hi = hi.to_vec();
        let pairs: Vec<_> = guard
            .iter()
            .filter(|(k, _)| **k >= lo && **k < hi)
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        drop(guard);
        Ok(Box::new(pairs.into_iter()))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Err(StoreError::Unsupported(
            "bench MemStore: no transaction support".to_string(),
        ))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Unsupported(
            "bench MemStore: no snapshot support".to_string(),
        ))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let guard = self.0.lock().expect("bench MemStore lock poisoned");
        let mut pairs: Vec<(Vec<u8>, Vec<u8>)> =
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        drop(guard);
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        Ok(Box::new(pairs.into_iter().map(Ok)))
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_store() -> EncryptedKv<MemStore, StaticKey> {
    EncryptedKv::new(MemStore::default(), StaticKey::from_array([0x42u8; 32]))
}

// ── Benchmarks ────────────────────────────────────────────────────────────────

fn bench_encrypt_put(c: &mut Criterion) {
    let mut group = c.benchmark_group("encrypt_put");

    for size in [64usize, 4_096, 65_536] {
        let data = vec![0u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
            let store = make_store();
            let mut i = 0u64;
            b.iter(|| {
                let key = i.to_le_bytes();
                store.put(&key, data).expect("bench encrypt put failed");
                i += 1;
            });
        });
    }

    group.finish();
}

fn bench_encrypt_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("encrypt_get");

    for size in [64usize, 4_096, 65_536] {
        let data = vec![0u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
            let store = make_store();
            store
                .put(b"bench_key", data)
                .expect("bench encrypt pre-populate failed");
            b.iter(|| {
                let _ = store.get(b"bench_key").expect("bench encrypt get failed");
            });
        });
    }

    group.finish();
}

// ── derive_cell_id latency ────────────────────────────────────────────────────

/// Benchmark `derive_cell_id` (BLAKE3 of raw key bytes) across several key
/// lengths to characterise the AAD derivation overhead.
///
/// `derive_cell_id` is called on every `EncryptedKv::put` and `get`; keeping
/// it fast matters for high-throughput workloads.
fn bench_derive_cell_id(c: &mut Criterion) {
    let mut group = c.benchmark_group("derive_cell_id");

    for key_len in [8usize, 32, 128] {
        let key_bytes: Vec<u8> = (0..key_len as u8).collect();
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(key_len),
            &key_bytes,
            |b, key| {
                b.iter(|| {
                    // BLAKE3 hash of key bytes → 32-byte cell ID used as AAD.
                    let _cell_id = derive_cell_id(std::hint::black_box(key));
                });
            },
        );
    }

    // Batch variant: derive 1 000 cell IDs in a single iter call to amortise
    // criterion overhead and measure aggregate throughput.
    group.bench_function("batch_1000_keys_32bytes", |b| {
        let keys: Vec<Vec<u8>> = (0u32..1000).map(|i| i.to_le_bytes().to_vec()).collect();
        b.iter(|| {
            for key in &keys {
                let _ = derive_cell_id(std::hint::black_box(key));
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_encrypt_put,
    bench_encrypt_get,
    bench_derive_cell_id
);
criterion_main!(benches);

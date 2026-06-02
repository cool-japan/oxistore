//! Write-heavy benchmark suite for `oxistore-kv-fjall`.
//!
//! Measures sequential-insert throughput and batched-write (transaction)
//! throughput to characterise the fjall LSM backend under write load.
//! Run with:
//!   cargo bench -p oxistore-kv-fjall
#![forbid(unsafe_code)]

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use oxistore_core::KvStore;
use oxistore_kv_fjall::FjallStore;
use std::env;

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

fn temp_path(label: &str) -> std::path::PathBuf {
    env::temp_dir().join(format!(
        "oxistore_fjall_bench_{}_{}",
        label,
        std::process::id(),
    ))
}

fn make_key(i: u64) -> Vec<u8> {
    i.to_be_bytes().to_vec()
}

fn make_value(i: u64) -> Vec<u8> {
    // 128-byte value: index repeated to fill
    let mut v = vec![0u8; 128];
    let bytes = i.to_be_bytes();
    for (chunk, &b) in v.chunks_mut(8).zip(bytes.iter().cycle()) {
        chunk[0] = b;
    }
    v
}

// --------------------------------------------------------------------------
// Sequential individual puts
// --------------------------------------------------------------------------

fn bench_sequential_puts(c: &mut Criterion) {
    let mut group = c.benchmark_group("fjall_sequential_puts");

    for n in [100u64, 1_000, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let path = temp_path(&format!("seq_{n}"));
                    FjallStore::open(&path).expect("bench open")
                },
                |store| {
                    for i in 0..n {
                        store.put(&make_key(i), &make_value(i)).expect("bench put");
                    }
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Batched writes via transactions
// --------------------------------------------------------------------------

fn bench_batched_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("fjall_batched_writes");

    for n in [100u64, 1_000, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let path = temp_path(&format!("batch_{n}"));
                    FjallStore::open(&path).expect("bench open")
                },
                |store| {
                    let mut txn = store.transaction().expect("bench txn");
                    for i in 0..n {
                        txn.put(&make_key(i), &make_value(i))
                            .expect("bench txn put");
                    }
                    txn.commit().expect("bench commit");
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Random-order puts (worst case for LSM bloom filters)
// --------------------------------------------------------------------------

fn bench_random_puts(c: &mut Criterion) {
    let mut group = c.benchmark_group("fjall_random_puts");

    // Simple deterministic LCG to avoid pulling in `rand`.
    fn lcg_next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state
    }

    for n in [100u64, 1_000, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let keys: Vec<u64> = {
                let mut state = 0xdead_beef_u64;
                (0..n).map(|_| lcg_next(&mut state) % (n * 4)).collect()
            };
            b.iter_batched(
                || {
                    let path = temp_path(&format!("rand_{n}"));
                    (FjallStore::open(&path).expect("bench open"), keys.clone())
                },
                |(store, ks)| {
                    for (i, k) in ks.iter().enumerate() {
                        store
                            .put(&k.to_be_bytes(), &make_value(i as u64))
                            .expect("bench put");
                    }
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Mixed read/write (80 % writes, 20 % reads)
// --------------------------------------------------------------------------

fn bench_mixed_rw(c: &mut Criterion) {
    let mut group = c.benchmark_group("fjall_mixed_80w_20r");

    fn lcg_next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state
    }

    for n in [1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let path = temp_path(&format!("mixed_{n}"));
                    let store = FjallStore::open(&path).expect("bench open");
                    // Pre-seed the store with half the keys.
                    for i in 0..(n / 2) {
                        store.put(&make_key(i), &make_value(i)).expect("seed");
                    }
                    let mut state = 0xcafe_u64;
                    let ops: Vec<(bool, u64)> = (0..n)
                        .map(|_| {
                            let is_write = lcg_next(&mut state) % 10 < 8;
                            let key = lcg_next(&mut state) % n;
                            (is_write, key)
                        })
                        .collect();
                    (store, ops)
                },
                |(store, ops)| {
                    for (is_write, k) in ops {
                        if is_write {
                            store.put(&make_key(k), &make_value(k)).expect("put");
                        } else {
                            let _ = store.get(&make_key(k)).expect("get");
                        }
                    }
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Range scan throughput
// --------------------------------------------------------------------------

fn bench_range_scan(c: &mut Criterion) {
    let n: u64 = 10_000;
    let path = temp_path("range_scan_fixed");
    let store = FjallStore::open(&path).expect("bench open");
    for i in 0..n {
        store.put(&make_key(i), &make_value(i)).expect("seed");
    }

    let mut group = c.benchmark_group("fjall_range_scan");
    for width in [100u64, 1_000, 10_000] {
        group.throughput(Throughput::Elements(width));
        group.bench_with_input(BenchmarkId::from_parameter(width), &width, |b, &w| {
            let lo = make_key(0);
            let hi = make_key(w);
            b.iter(|| {
                let iter = store.range(&lo, &hi).expect("range");
                let count = iter.count();
                assert!(count > 0);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_sequential_puts,
    bench_batched_writes,
    bench_random_puts,
    bench_mixed_rw,
    bench_range_scan,
);
criterion_main!(benches);

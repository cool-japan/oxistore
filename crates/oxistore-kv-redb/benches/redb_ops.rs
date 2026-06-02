//! Criterion benchmark suite for `oxistore-kv-redb`.
//!
//! Covers write-heavy, read-heavy, and range-scan workloads to characterise
//! the redb backend under realistic mixed access patterns.
//!
//! Run with:
//!   cargo bench -p oxistore-kv-redb
#![forbid(unsafe_code)]

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use oxistore_core::KvStore;
use oxistore_kv_redb::RedbStore;
use std::env;

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

fn temp_path(label: &str) -> std::path::PathBuf {
    env::temp_dir().join(format!(
        "oxistore_redb_bench_{}_{}",
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

/// Minimal deterministic LCG for workload generation (avoids pulling in `rand`).
fn lcg_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

// --------------------------------------------------------------------------
// Write-heavy: sequential individual puts
// --------------------------------------------------------------------------

fn bench_write_heavy(c: &mut Criterion) {
    let mut group = c.benchmark_group("redb_write_heavy");

    for n in [100u64, 1_000, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let path = temp_path(&format!("write_heavy_{n}"));
                    RedbStore::open(&path).expect("bench open")
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
// Read-heavy: 80 % reads, 20 % writes on pre-seeded store
// --------------------------------------------------------------------------

fn bench_read_heavy(c: &mut Criterion) {
    let mut group = c.benchmark_group("redb_read_heavy_80r_20w");

    for n in [1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let path = temp_path(&format!("read_heavy_{n}"));
                    let store = RedbStore::open(&path).expect("bench open");
                    // Pre-populate the entire key space so reads are not cold misses.
                    for i in 0..n {
                        store.put(&make_key(i), &make_value(i)).expect("seed");
                    }
                    let mut state = 0xbabe_u64;
                    // 80 % reads, 20 % writes
                    let ops: Vec<(bool, u64)> = (0..n)
                        .map(|_| {
                            let is_read = lcg_next(&mut state) % 10 < 8;
                            let key = lcg_next(&mut state) % n;
                            (is_read, key)
                        })
                        .collect();
                    (store, ops)
                },
                |(store, ops)| {
                    for (is_read, k) in ops {
                        if is_read {
                            let _ = store.get(&make_key(k)).expect("get");
                        } else {
                            store.put(&make_key(k), &make_value(k)).expect("put");
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
// Range scan: paginated streaming over pre-seeded table
// --------------------------------------------------------------------------

fn bench_range_scan(c: &mut Criterion) {
    let n: u64 = 10_000;
    let path = temp_path("range_scan_fixed");
    let store = RedbStore::open(&path).expect("bench open");
    for i in 0..n {
        store.put(&make_key(i), &make_value(i)).expect("seed");
    }

    let mut group = c.benchmark_group("redb_range_scan");
    for width in [100u64, 1_000, 10_000] {
        group.throughput(Throughput::Elements(width));
        group.bench_with_input(BenchmarkId::from_parameter(width), &width, |b, &w| {
            let lo = make_key(0);
            let hi = make_key(w);
            b.iter(|| {
                let count = store.range(&lo, &hi).expect("range").count();
                assert!(count > 0);
            });
        });
    }
    group.finish();
}

// --------------------------------------------------------------------------
// Full-table iter: paginated scan over pre-seeded table
// --------------------------------------------------------------------------

fn bench_full_iter(c: &mut Criterion) {
    let n: u64 = 5_000;
    let path = temp_path("full_iter_fixed");
    let store = RedbStore::open(&path).expect("bench open");
    for i in 0..n {
        store.put(&make_key(i), &make_value(i)).expect("seed");
    }

    let mut group = c.benchmark_group("redb_full_iter");
    group.throughput(Throughput::Elements(n));
    group.bench_function("iter_5k", |b| {
        b.iter(|| {
            let count = store.iter().expect("iter").count();
            assert_eq!(count, n as usize);
        });
    });
    group.finish();
}

// --------------------------------------------------------------------------
// Prefix scan
// --------------------------------------------------------------------------

fn bench_prefix_scan(c: &mut Criterion) {
    let store = RedbStore::open_in_memory().expect("bench open");
    // Insert 2 000 keys under "user:" prefix and 1 000 under "order:".
    for i in 0u64..2_000 {
        let key = format!("user:{:08}", i).into_bytes();
        store.put(&key, &make_value(i)).expect("seed user");
    }
    for i in 0u64..1_000 {
        let key = format!("order:{:08}", i).into_bytes();
        store.put(&key, &make_value(i)).expect("seed order");
    }

    let mut group = c.benchmark_group("redb_prefix_scan");
    group.throughput(Throughput::Elements(2_000));
    group.bench_function("prefix_user_2k", |b| {
        b.iter(|| {
            let count = store.prefix_scan(b"user:").expect("prefix_scan").count();
            assert_eq!(count, 2_000);
        });
    });
    group.finish();
}

// --------------------------------------------------------------------------
// Batch write throughput
// --------------------------------------------------------------------------

fn bench_batch_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("redb_batch_write");

    for n in [100u64, 1_000, 5_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let path = temp_path(&format!("batch_write_{n}"));
                    let keys: Vec<Vec<u8>> = (0..n).map(make_key).collect();
                    let vals: Vec<Vec<u8>> = (0..n).map(make_value).collect();
                    (RedbStore::open(&path).expect("bench open"), keys, vals)
                },
                |(store, keys, vals)| {
                    let pairs: Vec<(&[u8], &[u8])> = keys
                        .iter()
                        .zip(vals.iter())
                        .map(|(k, v)| (k.as_slice(), v.as_slice()))
                        .collect();
                    store.batch_write(&pairs).expect("batch_write");
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Snapshot creation latency
// --------------------------------------------------------------------------

fn bench_snapshot_creation(c: &mut Criterion) {
    // Pre-seed a store with 5 000 entries to show snapshot() is O(1).
    let store = RedbStore::open_in_memory().expect("bench open");
    for i in 0u64..5_000 {
        store.put(&make_key(i), &make_value(i)).expect("seed");
    }

    let mut group = c.benchmark_group("redb_snapshot");
    group.bench_function("snapshot_creation_5k_entries", |b| {
        b.iter(|| {
            let snap = store.snapshot().expect("snapshot");
            // Touch snap to prevent optimisation
            let _ = snap.get(&make_key(0)).expect("snap get");
        });
    });
    group.finish();
}

// --------------------------------------------------------------------------
// Registration
// --------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_write_heavy,
    bench_read_heavy,
    bench_range_scan,
    bench_full_iter,
    bench_prefix_scan,
    bench_batch_write,
    bench_snapshot_creation,
);
criterion_main!(benches);

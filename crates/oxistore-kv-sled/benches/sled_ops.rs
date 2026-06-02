//! Benchmark suite for `oxistore-kv-sled`.
//!
//! Measures write-heavy, read-heavy, and mixed workload throughput for the
//! sled backend.
//!
//! Run with:
//!   cargo bench -p oxistore-kv-sled
#![forbid(unsafe_code)]

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use oxistore_core::KvStore;
use oxistore_kv_sled::SledStore;
use std::env;

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

fn temp_path(label: &str) -> std::path::PathBuf {
    env::temp_dir().join(format!(
        "oxistore_sled_bench_{}_{}",
        label,
        std::process::id(),
    ))
}

fn make_key(i: u64) -> Vec<u8> {
    i.to_be_bytes().to_vec()
}

fn make_value(i: u64) -> Vec<u8> {
    // 128-byte value: index bytes repeated to fill.
    let mut v = vec![0u8; 128];
    let bytes = i.to_be_bytes();
    for (chunk, &b) in v.chunks_mut(8).zip(bytes.iter().cycle()) {
        chunk[0] = b;
    }
    v
}

// --------------------------------------------------------------------------
// Write-heavy: insert 1000 entries, measure throughput
// --------------------------------------------------------------------------

fn bench_write_heavy(c: &mut Criterion) {
    const N: u64 = 1_000;
    let mut group = c.benchmark_group("sled_write_heavy");
    group.throughput(Throughput::Elements(N));

    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, &n| {
        b.iter_batched(
            || {
                let path = temp_path("write");
                SledStore::open(&path).expect("bench open")
            },
            |store| {
                for i in 0..n {
                    store.put(&make_key(i), &make_value(i)).expect("bench put");
                }
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

// --------------------------------------------------------------------------
// Read-heavy: pre-populate 1000 entries, bench 1000 get() calls
// --------------------------------------------------------------------------

fn bench_read_heavy(c: &mut Criterion) {
    const N: u64 = 1_000;
    let mut group = c.benchmark_group("sled_read_heavy");
    group.throughput(Throughput::Elements(N));

    // Pre-populate a shared store for all read iterations.
    let path = temp_path("read");
    let store = SledStore::open(&path).expect("bench open");
    for i in 0..N {
        store.put(&make_key(i), &make_value(i)).expect("seed");
    }

    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, &n| {
        b.iter(|| {
            for i in 0..n {
                let _ = store.get(&make_key(i)).expect("bench get");
            }
        });
    });

    group.finish();
}

// --------------------------------------------------------------------------
// Mixed: interleave 500 puts + 500 gets
// --------------------------------------------------------------------------

fn bench_mixed(c: &mut Criterion) {
    const PUTS: u64 = 500;
    const GETS: u64 = 500;
    const TOTAL: u64 = PUTS + GETS;
    let mut group = c.benchmark_group("sled_mixed");
    group.throughput(Throughput::Elements(TOTAL));

    group.bench_with_input(BenchmarkId::from_parameter(TOTAL), &TOTAL, |b, _| {
        b.iter_batched(
            || {
                let path = temp_path("mixed");
                let store = SledStore::open(&path).expect("bench open");
                // Pre-seed with GETS keys so get() always hits.
                for i in 0..GETS {
                    store.put(&make_key(i), &make_value(i)).expect("seed");
                }
                store
            },
            |store| {
                // 500 puts (new keys starting at offset GETS).
                for i in 0..PUTS {
                    store
                        .put(&make_key(GETS + i), &make_value(GETS + i))
                        .expect("bench put");
                }
                // 500 gets on the pre-seeded keys.
                for i in 0..GETS {
                    let _ = store.get(&make_key(i)).expect("bench get");
                }
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_write_heavy, bench_read_heavy, bench_mixed);
criterion_main!(benches);

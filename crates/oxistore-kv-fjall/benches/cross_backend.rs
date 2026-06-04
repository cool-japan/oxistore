//! Cross-backend comparison benchmark: fjall vs redb vs sled.
//!
//! Compares three OxiStore KV backends on three identical workloads:
//!   1. 1 000-key write burst (sequential individual puts)
//!   2. 1 000-key sequential read (all keys present, ascending order)
//!   3. 1 000-key random read (all keys present, pseudo-random access order)
//!
//! Run with:
//!   cargo bench -p oxistore-kv-fjall --bench cross_backend
#![forbid(unsafe_code)]

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use oxistore_core::KvStore;
use oxistore_kv_fjall::FjallStore;
use oxistore_kv_redb::RedbStore;
use oxistore_kv_sled::SledStore;
use std::env;

// --------------------------------------------------------------------------
// Shared constants
// --------------------------------------------------------------------------

const N: u64 = 1_000;

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

fn temp_dir(backend: &str, bench: &str) -> std::path::PathBuf {
    env::temp_dir().join(format!(
        "oxistore_xback_{}_{}_{}_{}",
        backend,
        bench,
        std::process::id(),
        // Nanosecond timestamp avoids collisions between PerIteration runs.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0),
    ))
}

fn make_key(i: u64) -> Vec<u8> {
    i.to_be_bytes().to_vec()
}

fn make_value(i: u64) -> Vec<u8> {
    // 64-byte value — small enough not to dominate I/O timings.
    let mut v = vec![0u8; 64];
    let bytes = i.to_be_bytes();
    for (chunk, &b) in v.chunks_mut(8).zip(bytes.iter().cycle()) {
        chunk[0] = b;
    }
    v
}

/// Minimal deterministic LCG — avoids pulling in `rand`.
fn lcg_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

/// Pre-generate a pseudo-random access sequence over `[0, n)`.
fn random_access_seq(n: u64) -> Vec<u64> {
    let mut state = 0xdead_cafe_u64;
    (0..n).map(|_| lcg_next(&mut state) % n).collect()
}

// --------------------------------------------------------------------------
// Workload 1 — 1 000-key write burst
// --------------------------------------------------------------------------

fn bench_write_burst(c: &mut Criterion) {
    let mut group = c.benchmark_group("cross_backend_write_burst_1k");
    group.throughput(Throughput::Elements(N));

    // ── fjall ───────────────────────────────────────────────────────────────
    group.bench_function(BenchmarkId::new("fjall", N), |b| {
        b.iter_batched(
            || {
                let path = temp_dir("fjall", "wburst");
                FjallStore::open(&path).expect("fjall open")
            },
            |store| {
                for i in 0..N {
                    store.put(&make_key(i), &make_value(i)).expect("fjall put");
                }
            },
            BatchSize::PerIteration,
        );
    });

    // ── redb ────────────────────────────────────────────────────────────────
    group.bench_function(BenchmarkId::new("redb", N), |b| {
        b.iter_batched(
            || {
                let path = temp_dir("redb", "wburst").with_extension("redb");
                RedbStore::open(&path).expect("redb open")
            },
            |store| {
                for i in 0..N {
                    store.put(&make_key(i), &make_value(i)).expect("redb put");
                }
            },
            BatchSize::PerIteration,
        );
    });

    // ── sled ────────────────────────────────────────────────────────────────
    group.bench_function(BenchmarkId::new("sled", N), |b| {
        b.iter_batched(
            || {
                let path = temp_dir("sled", "wburst");
                SledStore::open(&path).expect("sled open")
            },
            |store| {
                for i in 0..N {
                    store.put(&make_key(i), &make_value(i)).expect("sled put");
                }
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

// --------------------------------------------------------------------------
// Workload 2 — 1 000-key sequential read
// --------------------------------------------------------------------------

fn bench_sequential_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("cross_backend_sequential_read_1k");
    group.throughput(Throughput::Elements(N));

    // ── fjall ───────────────────────────────────────────────────────────────
    {
        let path = temp_dir("fjall", "seqread");
        let store = FjallStore::open(&path).expect("fjall open");
        for i in 0..N {
            store.put(&make_key(i), &make_value(i)).expect("seed");
        }
        group.bench_function(BenchmarkId::new("fjall", N), |b| {
            b.iter(|| {
                for i in 0..N {
                    let _ = store.get(&make_key(i)).expect("fjall get");
                }
            });
        });
    }

    // ── redb ────────────────────────────────────────────────────────────────
    {
        let path = temp_dir("redb", "seqread").with_extension("redb");
        let store = RedbStore::open(&path).expect("redb open");
        for i in 0..N {
            store.put(&make_key(i), &make_value(i)).expect("seed");
        }
        group.bench_function(BenchmarkId::new("redb", N), |b| {
            b.iter(|| {
                for i in 0..N {
                    let _ = store.get(&make_key(i)).expect("redb get");
                }
            });
        });
    }

    // ── sled ────────────────────────────────────────────────────────────────
    {
        let path = temp_dir("sled", "seqread");
        let store = SledStore::open(&path).expect("sled open");
        for i in 0..N {
            store.put(&make_key(i), &make_value(i)).expect("seed");
        }
        group.bench_function(BenchmarkId::new("sled", N), |b| {
            b.iter(|| {
                for i in 0..N {
                    let _ = store.get(&make_key(i)).expect("sled get");
                }
            });
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Workload 3 — 1 000-key random read
// --------------------------------------------------------------------------

fn bench_random_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("cross_backend_random_read_1k");
    group.throughput(Throughput::Elements(N));

    let access_seq = random_access_seq(N);

    // ── fjall ───────────────────────────────────────────────────────────────
    {
        let path = temp_dir("fjall", "randread");
        let store = FjallStore::open(&path).expect("fjall open");
        for i in 0..N {
            store.put(&make_key(i), &make_value(i)).expect("seed");
        }
        let seq = access_seq.clone();
        group.bench_function(BenchmarkId::new("fjall", N), |b| {
            b.iter(|| {
                for &k in &seq {
                    let _ = store.get(&make_key(k)).expect("fjall rand get");
                }
            });
        });
    }

    // ── redb ────────────────────────────────────────────────────────────────
    {
        let path = temp_dir("redb", "randread").with_extension("redb");
        let store = RedbStore::open(&path).expect("redb open");
        for i in 0..N {
            store.put(&make_key(i), &make_value(i)).expect("seed");
        }
        let seq = access_seq.clone();
        group.bench_function(BenchmarkId::new("redb", N), |b| {
            b.iter(|| {
                for &k in &seq {
                    let _ = store.get(&make_key(k)).expect("redb rand get");
                }
            });
        });
    }

    // ── sled ────────────────────────────────────────────────────────────────
    {
        let path = temp_dir("sled", "randread");
        let store = SledStore::open(&path).expect("sled open");
        for i in 0..N {
            store.put(&make_key(i), &make_value(i)).expect("seed");
        }
        let seq = access_seq.clone();
        group.bench_function(BenchmarkId::new("sled", N), |b| {
            b.iter(|| {
                for &k in &seq {
                    let _ = store.get(&make_key(k)).expect("sled rand get");
                }
            });
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Registration
// --------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_write_burst,
    bench_sequential_read,
    bench_random_read,
);
criterion_main!(benches);

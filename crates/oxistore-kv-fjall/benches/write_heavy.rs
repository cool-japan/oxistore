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

// --------------------------------------------------------------------------
// LSM write-amplification under sustained write workloads
// --------------------------------------------------------------------------
//
// We measure the time required to insert `n` records sequentially, then
// re-insert them all (overwrites), then delete half, and measure the total
// elapsed wall-clock time for the full write-heavy lifecycle.  The ratio of
// bytes flushed vs bytes inserted provides an indirect write-amplification
// signal at the application level; true LSM WA requires instrumentation
// inside fjall itself, which is not exposed through the public API.

fn bench_write_amplification(c: &mut Criterion) {
    let mut group = c.benchmark_group("fjall_write_amplification");
    // Reduce measurement time so CI doesn't time out on larger sizes.
    group.sample_size(10);

    for n in [500u64, 2_000, 5_000] {
        group.throughput(Throughput::Elements(n * 2)); // writes + rewrites
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let path = temp_path(&format!("wamp_{n}"));
                    FjallStore::open(&path).expect("bench open")
                },
                |store| {
                    // Phase 1: initial writes
                    for i in 0..n {
                        store
                            .put(&make_key(i), &make_value(i))
                            .expect("initial put");
                    }
                    // Phase 2: overwrites (each triggers a new SST record → amplification)
                    for i in 0..n {
                        store
                            .put(&make_key(i), &make_value(i.wrapping_add(1)))
                            .expect("overwrite put");
                    }
                    // Phase 3: delete half the keys
                    for i in 0..(n / 2) {
                        store.delete(&make_key(i)).expect("delete");
                    }
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Range scan throughput with varying scan widths and key distributions
// --------------------------------------------------------------------------
//
// Seeds the store once with `n` sequential keys, then measures how long it
// takes to perform range scans over three different widths (narrow / medium /
// full) and two key distributions (sequential prefix and strided/sparse).

fn bench_range_scan_widths(c: &mut Criterion) {
    // Shared setup: seed one store for sequential-key scans.
    let n: u64 = 10_000;
    let path_seq = temp_path("rs_widths_seq");
    let store_seq = FjallStore::open(&path_seq).expect("bench open seq");
    for i in 0..n {
        store_seq
            .put(&make_key(i), &make_value(i))
            .expect("seed seq");
    }

    // Strided store: keys 0, 10, 20, … 9990 (1000 total, gaps of 10)
    let path_strided = temp_path("rs_widths_strided");
    let store_strided = FjallStore::open(&path_strided).expect("bench open strided");
    for i in (0..n).step_by(10) {
        store_strided
            .put(&make_key(i), &make_value(i))
            .expect("seed strided");
    }

    let mut group = c.benchmark_group("fjall_range_scan_widths");

    // Sequential distribution: widths of 100, 1000, 10000
    for width in [100u64, 1_000, 10_000] {
        group.throughput(Throughput::Elements(width));
        group.bench_with_input(BenchmarkId::new("sequential", width), &width, |b, &w| {
            let lo = make_key(0);
            let hi = make_key(w);
            b.iter(|| {
                let count = store_seq.range(&lo, &hi).expect("range seq").count();
                assert!(count > 0, "seq range count must be > 0");
            });
        });
    }

    // Strided distribution: scan the full key space (sparsely populated)
    for width in [100u64, 1_000, 10_000] {
        group.throughput(Throughput::Elements(width / 10 + 1));
        group.bench_with_input(BenchmarkId::new("strided", width), &width, |b, &w| {
            let lo = make_key(0);
            let hi = make_key(w);
            b.iter(|| {
                let _count = store_strided
                    .range(&lo, &hi)
                    .expect("range strided")
                    .count();
            });
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Compaction impact on read latency
// --------------------------------------------------------------------------
//
// Inserts `n` keys, triggers a manual compaction, then measures single-key
// read latency both before and after compaction to expose any read-latency
// delta.  Requires `KvStore::compact()` which is available on `FjallStore`.

fn bench_compaction_read_impact(c: &mut Criterion) {
    use oxistore_kv_fjall::FjallStoreBuilder;

    let n: u64 = 5_000;
    let mut group = c.benchmark_group("fjall_compaction_read_impact");
    group.sample_size(20);

    // ── Before compaction ────────────────────────────────────────────────────
    {
        let path = temp_path("compact_before");
        let store = FjallStore::open(&path).expect("bench open before");
        // Insert n keys to create multiple SST levels.
        for i in 0..n {
            store.put(&make_key(i), &make_value(i)).expect("seed");
        }

        group.bench_function("read_before_compaction", |b| {
            b.iter(|| {
                for i in [0u64, n / 4, n / 2, 3 * n / 4, n - 1] {
                    let _ = store.get(&make_key(i)).expect("get");
                }
            });
        });
    }

    // ── After compaction ─────────────────────────────────────────────────────
    {
        let path = temp_path("compact_after");
        let store = FjallStoreBuilder::new()
            .bloom_filter_bits_per_key(10.0)
            .build(&path)
            .expect("bench build after");
        for i in 0..n {
            store.put(&make_key(i), &make_value(i)).expect("seed");
        }
        store.compact().expect("compact");

        group.bench_function("read_after_compaction", |b| {
            b.iter(|| {
                for i in [0u64, n / 4, n / 2, 3 * n / 4, n - 1] {
                    let _ = store.get(&make_key(i)).expect("get");
                }
            });
        });
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Bloom filter memory / false-positive impact across bits-per-key settings
// --------------------------------------------------------------------------
//
// Seeds a store with `n` keys, then for each bloom filter bits-per-key
// value in {5, 10, 15, 20} measures the average time of looking up
// `n_miss` keys that are guaranteed **not** to exist (all-miss workload).
// Fewer false positives → fewer unnecessary disk reads → faster miss lookup.

fn bench_bloom_filter_bits(c: &mut Criterion) {
    use oxistore_kv_fjall::FjallStoreBuilder;

    let n: u64 = 5_000;
    let n_miss: u64 = 200;
    // Miss keys start far above the inserted range to guarantee all misses.
    let miss_base: u64 = n * 100;

    let mut group = c.benchmark_group("fjall_bloom_filter_bits_per_key");
    group.sample_size(20);

    for bits in [5u32, 10, 15, 20] {
        let path = temp_path(&format!("bloom_bpk_{bits}"));
        let store = FjallStoreBuilder::new()
            .bloom_filter_bits_per_key(bits as f32)
            .build(&path)
            .expect("build bloom store");
        for i in 0..n {
            store.put(&make_key(i), &make_value(i)).expect("seed");
        }

        group.throughput(Throughput::Elements(n_miss));
        group.bench_with_input(
            BenchmarkId::from_parameter(bits),
            &(bits, n_miss, miss_base),
            |b, &(_bits, n_miss, miss_base)| {
                b.iter(|| {
                    for j in 0..n_miss {
                        let _ = store.get(&make_key(miss_base + j)).expect("get miss");
                    }
                });
            },
        );
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Batch write throughput with varying batch sizes (10, 100, 1000, 10 000)
// --------------------------------------------------------------------------

fn bench_batch_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("fjall_batch_sizes");

    for batch_size in [10u64, 100, 1_000, 10_000] {
        group.throughput(Throughput::Elements(batch_size));
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &bs| {
                b.iter_batched(
                    || {
                        let path = temp_path(&format!("bsz_{bs}"));
                        let store = FjallStore::open(&path).expect("bench open");
                        // Build the batch payload once per iteration.
                        let pairs: Vec<(Vec<u8>, Vec<u8>)> =
                            (0..bs).map(|i| (make_key(i), make_value(i))).collect();
                        (store, pairs)
                    },
                    |(store, pairs)| {
                        let refs: Vec<(&[u8], &[u8])> = pairs
                            .iter()
                            .map(|(k, v)| (k.as_slice(), v.as_slice()))
                            .collect();
                        store.batch_write(&refs).expect("batch_write");
                    },
                    BatchSize::PerIteration,
                );
            },
        );
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
    bench_write_amplification,
    bench_range_scan_widths,
    bench_compaction_read_impact,
    bench_bloom_filter_bits,
    bench_batch_sizes,
);
criterion_main!(benches);

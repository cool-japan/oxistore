//! Benchmark suite for `oxistore-kv-sled`.
//!
//! Measures write-heavy, read-heavy, mixed, batch, prefix-scan,
//! compare-and-swap throughput and memory-pressure behaviour for the
//! sled backend.
//!
//! Run with:
//!   cargo bench -p oxistore-kv-sled
#![forbid(unsafe_code)]

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use oxistore_core::KvStore;
use oxistore_kv_sled::SledStore;
use std::env;
use std::sync::{atomic::AtomicU64, atomic::Ordering, Arc};

// --------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------

static BENCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_path(label: &str) -> std::path::PathBuf {
    let seq = BENCH_SEQ.fetch_add(1, Ordering::Relaxed);
    env::temp_dir().join(format!(
        "oxistore_sled_bench_{}_{}_{seq}",
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

/// Build a pre-populated store with `n` 128-byte values at sequential keys.
fn populated_store(label: &str, n: u64) -> SledStore {
    let store = SledStore::open(temp_path(label)).expect("bench open");
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..n).map(|i| (make_key(i), make_value(i))).collect();
    let refs: Vec<(&[u8], &[u8])> = pairs
        .iter()
        .map(|(k, v)| (k.as_slice(), v.as_slice()))
        .collect();
    store.batch_write(&refs).expect("seed batch_write");
    store
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

// --------------------------------------------------------------------------
// Delete throughput: 500 deletes on pre-populated keys
// --------------------------------------------------------------------------

fn bench_delete(c: &mut Criterion) {
    const N: u64 = 500;
    let mut group = c.benchmark_group("sled_delete");
    group.throughput(Throughput::Elements(N));

    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, &n| {
        b.iter_batched(
            || {
                let store = populated_store("delete", n);
                // Collect keys in advance to avoid alloc inside measurement.
                let keys: Vec<Vec<u8>> = (0..n).map(make_key).collect();
                (store, keys)
            },
            |(store, keys)| {
                for k in &keys {
                    store.delete(k).expect("bench delete");
                }
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

// --------------------------------------------------------------------------
// Batch write vs individual write
// --------------------------------------------------------------------------

fn bench_batch_vs_individual(c: &mut Criterion) {
    const N: u64 = 1_000;
    let mut group = c.benchmark_group("sled_batch_vs_individual");
    group.throughput(Throughput::Elements(N));

    // Individual writes: 1000 separate `put()` calls.
    group.bench_function("individual_put_1000", |b| {
        b.iter_batched(
            || SledStore::open(temp_path("individual")).expect("open"),
            |store| {
                for i in 0..N {
                    store.put(&make_key(i), &make_value(i)).expect("put");
                }
            },
            BatchSize::PerIteration,
        );
    });

    // Batch write: one `batch_write()` call with 1000 pairs.
    group.bench_function("batch_write_1000", |b| {
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..N).map(|i| (make_key(i), make_value(i))).collect();

        b.iter_batched(
            || {
                let store = SledStore::open(temp_path("batch")).expect("open");
                let refs: Vec<(&[u8], &[u8])> = pairs
                    .iter()
                    .map(|(k, v)| (k.as_slice(), v.as_slice()))
                    .collect();
                (store, refs)
            },
            |(store, refs)| {
                store.batch_write(&refs).expect("batch_write");
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

// --------------------------------------------------------------------------
// Batch delete vs individual delete
// --------------------------------------------------------------------------

fn bench_batch_delete_vs_individual(c: &mut Criterion) {
    const N: u64 = 1_000;
    let mut group = c.benchmark_group("sled_batch_delete_vs_individual");
    group.throughput(Throughput::Elements(N));

    // Individual deletes.
    group.bench_function("individual_delete_1000", |b| {
        b.iter_batched(
            || populated_store("indiv_del", N),
            |store| {
                for i in 0..N {
                    store.delete(&make_key(i)).expect("delete");
                }
            },
            BatchSize::PerIteration,
        );
    });

    // Batch delete.
    group.bench_function("batch_delete_1000", |b| {
        let keys: Vec<Vec<u8>> = (0..N).map(make_key).collect();

        b.iter_batched(
            || {
                let store = populated_store("batch_del", N);
                let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
                (store, refs)
            },
            |(store, refs)| {
                store.batch_delete(&refs).expect("batch_delete");
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

// --------------------------------------------------------------------------
// Prefix scan with varying selectivity
// --------------------------------------------------------------------------

fn bench_prefix_scan(c: &mut Criterion) {
    // N total keys split across `n_prefixes` equal-length prefix groups.
    const TOTAL: u64 = 10_000;

    let mut group = c.benchmark_group("sled_prefix_scan");

    for &n_prefixes in &[1u64, 10, 100, 1_000] {
        let keys_per_prefix = TOTAL / n_prefixes;
        group.throughput(Throughput::Elements(keys_per_prefix));

        group.bench_with_input(
            BenchmarkId::new("keys_per_prefix", keys_per_prefix),
            &(n_prefixes, keys_per_prefix),
            |b, &(n_pfx, kpp)| {
                // Build store once outside the iteration.
                let store = SledStore::open(temp_path(&format!("pfx_scan_{n_pfx}"))).expect("open");
                for pfx in 0..n_pfx {
                    for k in 0..kpp {
                        let key = format!("pfx{pfx:06}:{k:08}");
                        store.put(key.as_bytes(), b"v").expect("seed");
                    }
                }

                b.iter(|| {
                    // Scan the first prefix group.
                    let prefix = format!("pfx{:06}:", 0u64);
                    let _ = store
                        .prefix_scan(prefix.as_bytes())
                        .expect("prefix_scan")
                        .count();
                });
            },
        );
    }

    group.finish();
}

// --------------------------------------------------------------------------
// Memory usage under sustained writes
// --------------------------------------------------------------------------
//
// This is an approximate "memory pressure" benchmark: it measures the wall-
// clock time of 100k sequential puts with 256-byte values and records
// throughput.  Actual heap profiling is out of scope for criterion; use
// `heaptrack` or `dhat` for that.

fn bench_sustained_write_memory(c: &mut Criterion) {
    const N: u64 = 100_000;
    let mut group = c.benchmark_group("sled_sustained_write_memory_pressure");
    group.sample_size(10); // 100k inserts per sample is expensive
    group.throughput(Throughput::Elements(N));

    group.bench_function("100k_puts_256b_values", |b| {
        b.iter_batched(
            || SledStore::open(temp_path("sustained")).expect("open"),
            |store| {
                // 256-byte value
                let val = vec![0xABu8; 256];
                for i in 0..N {
                    store.put(&make_key(i), &val).expect("put");
                }
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

// --------------------------------------------------------------------------
// compare_and_swap under contention
// --------------------------------------------------------------------------

fn bench_cas_contention(c: &mut Criterion) {
    // N threads each perform M CAS operations on the *same* key.
    const THREADS: usize = 4;
    const OPS_PER_THREAD: u64 = 100;
    const TOTAL: u64 = THREADS as u64 * OPS_PER_THREAD;

    let mut group = c.benchmark_group("sled_cas_contention");
    group.throughput(Throughput::Elements(TOTAL));

    group.bench_function("4threads_100ops_each", |b| {
        b.iter_batched(
            || {
                let store = Arc::new(SledStore::open(temp_path("cas")).expect("open"));
                // Initialise the shared key.
                store.put(b"shared", b"0").expect("init");
                store
            },
            |store| {
                let mut handles = Vec::with_capacity(THREADS);
                for _ in 0..THREADS {
                    let s = Arc::clone(&store);
                    handles.push(std::thread::spawn(move || {
                        for _ in 0..OPS_PER_THREAD {
                            // Spin-CAS: read current, attempt swap.
                            loop {
                                let cur = s.get(b"shared").expect("get").unwrap_or_default();
                                let next = cur
                                    .iter()
                                    .fold(0u64, |acc, &b| acc * 256 + b as u64)
                                    .wrapping_add(1)
                                    .to_be_bytes()
                                    .to_vec();
                                if s.compare_and_swap(b"shared", Some(&cur), &next)
                                    .expect("cas")
                                {
                                    break;
                                }
                            }
                        }
                    }));
                }
                for h in handles {
                    h.join().expect("thread panicked");
                }
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

// --------------------------------------------------------------------------
// CAS throughput — no contention (sequential)
// --------------------------------------------------------------------------

fn bench_cas_sequential(c: &mut Criterion) {
    const N: u64 = 500;
    let mut group = c.benchmark_group("sled_cas_sequential");
    group.throughput(Throughput::Elements(N));

    group.bench_with_input(BenchmarkId::from_parameter(N), &N, |b, &n| {
        b.iter_batched(
            || {
                let store = SledStore::open(temp_path("cas_seq")).expect("open");
                store.put(b"k", b"0").expect("init");
                store
            },
            |store| {
                for i in 0..n {
                    let old = i.to_be_bytes();
                    let new = (i + 1).to_be_bytes();
                    let _ = store.compare_and_swap(b"k", Some(&old), &new).expect("cas");
                }
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_write_heavy,
    bench_read_heavy,
    bench_mixed,
    bench_delete,
    bench_batch_vs_individual,
    bench_batch_delete_vs_individual,
    bench_prefix_scan,
    bench_sustained_write_memory,
    bench_cas_contention,
    bench_cas_sequential,
);
criterion_main!(benches);

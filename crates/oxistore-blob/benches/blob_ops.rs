//! Criterion benchmarks for `oxistore-blob` operations.
//!
//! Covers:
//! - [`LocalBlobStore`] put/get throughput for 1 KiB, 1 MiB, and 100 MiB payloads.
//! - [`MemoryBlobStore`] concurrent read/write throughput.
//! - [`LocalBlobStore`] list performance with large directories.
//! - Atomic-rename write vs direct (non-atomic) write overhead.
//! - Streaming read ([`BlobStore::put_streaming`]) vs full-materialized
//!   ([`BlobStore::put`]) for large blobs.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use oxistore_blob::{BlobStore, LocalBlobStore, MemoryBlobStore};

use bytes::Bytes;
use std::path::PathBuf;
use std::sync::Arc;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Return a unique temporary directory for each benchmark invocation.
fn temp_bench_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "blob_bench_{tag}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ))
}

// ── LocalBlobStore::put benchmark ────────────────────────────────────────────
//
// Measures atomic-rename write throughput for 1 KiB, 1 MiB, and 100 MiB.

fn bench_local_blob_put(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("local_blob_put");

    // 1 KiB, 1 MiB, 100 MiB
    for size in [1_024usize, 1_048_576, 104_857_600] {
        let data = Bytes::from(vec![0xABu8; size]);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, payload| {
            let dir = temp_bench_dir("put");
            std::fs::create_dir_all(&dir).expect("create bench dir");
            let store = LocalBlobStore::new(&dir);
            let mut counter = 0u64;

            b.iter(|| {
                counter += 1;
                let key = format!("bench_{counter}");
                rt.block_on(async { store.put(&key, payload.clone()).await })
                    .expect("put");
            });

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    group.finish();
}

// ── LocalBlobStore::get benchmark ────────────────────────────────────────────

fn bench_local_blob_get(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("local_blob_get");

    for size in [1_024usize, 1_048_576, 104_857_600] {
        let data = Bytes::from(vec![0xCDu8; size]);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, payload| {
            let dir = temp_bench_dir("get");
            std::fs::create_dir_all(&dir).expect("create bench dir");
            let store = LocalBlobStore::new(&dir);

            // Pre-populate a single key to read in the loop.
            rt.block_on(async { store.put("bench_key", payload.clone()).await })
                .expect("pre-populate");

            b.iter(|| {
                rt.block_on(async { store.get("bench_key").await })
                    .expect("get")
            });

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    group.finish();
}

// ── MemoryBlobStore::put benchmark ───────────────────────────────────────────

fn bench_memory_blob_put(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("memory_blob_put");

    for size in [1_024usize, 65_536, 1_048_576] {
        let data = Bytes::from(vec![0xEFu8; size]);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, payload| {
            let store = MemoryBlobStore::new();
            let mut counter = 0u64;

            b.iter(|| {
                counter += 1;
                let key = format!("bench_{counter}");
                rt.block_on(async { store.put(&key, payload.clone()).await })
                    .expect("put");
            });
        });
    }

    group.finish();
}

// ── MemoryBlobStore concurrent read/write throughput ─────────────────────────
//
// Measures throughput when 4 writer tasks and 4 reader tasks run concurrently.
// The benchmark is single-iteration to capture the scheduling overhead of the
// multi-thread runtime; Criterion measures wall-clock time.

fn bench_memory_concurrent(c: &mut Criterion) {
    // Multi-thread runtime so tokio actually runs tasks concurrently.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("memory_concurrent");
    let ops_per_task = 200usize;
    let payload_size = 4_096usize;
    let num_tasks = 4usize;

    group.throughput(Throughput::Elements((ops_per_task * num_tasks * 2) as u64));

    group.bench_function("4w_4r_4kib", |b| {
        b.iter(|| {
            let store = Arc::new(MemoryBlobStore::new());

            // Seed some keys for readers.
            rt.block_on(async {
                for i in 0..ops_per_task {
                    store
                        .put(&format!("seed_{i}"), Bytes::from(vec![0u8; payload_size]))
                        .await
                        .expect("seed put");
                }
            });

            rt.block_on(async {
                let store_w = Arc::clone(&store);
                let store_r = Arc::clone(&store);

                // Spawn writers.
                let writers: Vec<_> = (0..num_tasks)
                    .map(|t| {
                        let s = Arc::clone(&store_w);
                        let data = Bytes::from(vec![(t as u8).wrapping_mul(17); payload_size]);
                        tokio::spawn(async move {
                            for i in 0..ops_per_task {
                                let key = format!("w{t}_{i}");
                                s.put(&key, data.clone()).await.expect("write");
                            }
                        })
                    })
                    .collect();

                // Spawn readers.
                let readers: Vec<_> = (0..num_tasks)
                    .map(|_| {
                        let s = Arc::clone(&store_r);
                        tokio::spawn(async move {
                            for i in 0..ops_per_task {
                                let key = format!("seed_{i}");
                                let _ = s.get(&key).await;
                            }
                        })
                    })
                    .collect();

                for jh in writers.into_iter().chain(readers) {
                    jh.await.expect("task panicked");
                }
            });
        });
    });

    group.finish();
}

// ── LocalBlobStore list performance with large directories ────────────────────
//
// Populates a directory with N keys, then benchmarks `list("")`.

fn bench_local_list_large(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("local_list");

    for n_keys in [100usize, 1000, 5000] {
        let dir = temp_bench_dir("list");
        std::fs::create_dir_all(&dir).expect("create bench dir");
        let store = LocalBlobStore::new(&dir);

        // Pre-populate with small payloads.
        rt.block_on(async {
            for i in 0..n_keys {
                store
                    .put(&format!("key_{i:06}"), Bytes::from(b"x".as_ref()))
                    .await
                    .expect("put");
            }
        });

        group.throughput(Throughput::Elements(n_keys as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n_keys), &n_keys, |b, _n| {
            b.iter(|| rt.block_on(async { store.list("").await }).expect("list"));
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    group.finish();
}

// ── Atomic-rename overhead vs direct write ────────────────────────────────────
//
// Compares:
//   1. The normal `LocalBlobStore::put` (temp-file + rename).
//   2. A direct `tokio::fs::write` to the final path (no atomic guarantee).
//
// Both write a 256 KiB payload.

fn bench_rename_vs_direct(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let payload_size = 262_144usize; // 256 KiB
    let data = Bytes::from(vec![0x42u8; payload_size]);

    let mut group = c.benchmark_group("write_overhead_256kib");
    group.throughput(Throughput::Bytes(payload_size as u64));

    // 1. Atomic rename (LocalBlobStore::put).
    let dir_atomic = temp_bench_dir("atomic");
    std::fs::create_dir_all(&dir_atomic).expect("create dir");
    let store = LocalBlobStore::new(&dir_atomic);
    let mut counter = 0u64;

    group.bench_function("atomic_rename", |b| {
        b.iter(|| {
            counter += 1;
            let key = format!("k{counter}");
            rt.block_on(async { store.put(&key, data.clone()).await })
                .expect("put");
        });
    });

    let _ = std::fs::remove_dir_all(&dir_atomic);

    // 2. Direct write (no rename).
    let dir_direct = temp_bench_dir("direct");
    std::fs::create_dir_all(&dir_direct).expect("create dir");
    let mut counter2 = 0u64;
    let data2 = data.clone();
    let dir_direct2 = dir_direct.clone();

    group.bench_function("direct_write", move |b| {
        b.iter(|| {
            counter2 += 1;
            let path = dir_direct2.join(format!("k{counter2}"));
            rt.block_on(async { tokio::fs::write(&path, &data2).await })
                .expect("write");
        });
    });

    let _ = std::fs::remove_dir_all(&dir_direct);

    group.finish();
}

// ── Streaming put vs one-shot put ─────────────────────────────────────────────
//
// Compares `BlobStore::put_streaming` (reads from an `AsyncRead`, hashes
// incrementally) vs `BlobStore::put_cas` (hashes the whole `Bytes` at once)
// for a 4 MiB payload.  Both go through [`MemoryBlobStore`] so the
// filesystem I/O cost is eliminated from the comparison.

fn bench_streaming_vs_materialized(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let payload_size = 4 * 1_048_576usize; // 4 MiB
    let raw: Vec<u8> = (0..payload_size).map(|i| (i % 251) as u8).collect();
    let data = Bytes::from(raw.clone());

    let mut group = c.benchmark_group("put_4mib");
    group.throughput(Throughput::Bytes(payload_size as u64));

    // 1. Streaming via put_streaming.
    {
        let store = MemoryBlobStore::new();
        let raw_clone = raw.clone();
        group.bench_function("streaming", |b| {
            b.iter(|| {
                // Each iteration provides a fresh in-memory reader.
                let reader = std::io::Cursor::new(raw_clone.clone());
                rt.block_on(async { store.put_streaming(reader).await })
                    .expect("put_streaming");
            });
        });
    }

    // 2. One-shot put via put_cas.
    {
        let store = MemoryBlobStore::new();
        group.bench_function("one_shot_cas", |b| {
            b.iter(|| {
                rt.block_on(async { store.put_cas(data.clone()).await })
                    .expect("put_cas");
            });
        });
    }

    group.finish();
}

// ── criterion entry points ────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_local_blob_put,
    bench_local_blob_get,
    bench_memory_blob_put,
    bench_memory_concurrent,
    bench_local_list_large,
    bench_rename_vs_direct,
    bench_streaming_vs_materialized,
);
criterion_main!(benches);

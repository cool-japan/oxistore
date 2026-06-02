//! Criterion benchmarks for `oxistore-blob` operations.
//!
//! Covers `put` + `get` throughput for the [`LocalBlobStore`] at three
//! payload sizes: 1 KiB, 64 KiB, and 1 MiB.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use oxistore_blob::{BlobStore, LocalBlobStore, MemoryBlobStore};

use bytes::Bytes;
use std::path::PathBuf;

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

fn bench_local_blob_put(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut group = c.benchmark_group("local_blob_put");

    for size in [1_024usize, 65_536, 1_048_576] {
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

    for size in [1_024usize, 65_536, 1_048_576] {
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

// ── criterion entry points ────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_local_blob_put,
    bench_local_blob_get,
    bench_memory_blob_put
);
criterion_main!(benches);

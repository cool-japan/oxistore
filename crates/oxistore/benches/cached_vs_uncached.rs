//! Benchmark: LRU-cached store vs uncached store on repeated reads.
//!
//! Measures the throughput improvement of an LRU cache layer over raw redb
//! access for read-heavy workloads.  Writes go through both paths
//! unconditionally (cache is write-through).  The benchmark covers:
//!
//! - **Hot reads** — the same key repeated (100% cache hit rate).
//! - **Warm reads** — keys from a small working set that fits in the cache.
//! - **Cold reads** — random keys from a large set that exceeds the cache capacity.
//!
//! Run with:
//!   cargo bench --bench cached_vs_uncached --features kv-redb,cache

#![forbid(unsafe_code)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use oxistore::KvStore as _;

/// Number of keys pre-inserted into both stores.
const DATASET_SIZE: u64 = 1_000;
/// LRU cache capacity (in entries); intentionally fits the warm-read working set.
const CACHE_CAP: usize = 200;

fn seed_uncached_store(label: &str) -> (oxistore::BoxKvStore, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "oxistore_bench_uncached_{}_{}",
        label,
        std::process::id()
    ));
    let store = oxistore::open(&dir).expect("open uncached store");
    for i in 0u64..DATASET_SIZE {
        store
            .put(&i.to_le_bytes(), b"benchmark_read_value_01234567890")
            .expect("seed put");
    }
    (store, dir)
}

fn seed_cached_store(label: &str) -> (oxistore::CachedKvStore, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "oxistore_bench_cached_{}_{}",
        label,
        std::process::id()
    ));
    let store =
        oxistore::open_cached(oxistore::StoreKind::Redb, &dir, CACHE_CAP).expect("open_cached");
    for i in 0u64..DATASET_SIZE {
        store
            .put(&i.to_le_bytes(), b"benchmark_read_value_01234567890")
            .expect("seed put");
    }
    (store, dir)
}

// ── Hot read (single key, always in cache after first access) ─────────────────

fn bench_hot_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("cached_vs_uncached/hot_read");
    group.throughput(Throughput::Elements(1));

    let hot_key = 42u64.to_le_bytes();

    let (uncached, dir_u) = seed_uncached_store("hot_uncached");
    group.bench_function(BenchmarkId::new("uncached", "redb"), |b| {
        b.iter(|| {
            let _ = uncached.get(&hot_key).expect("get");
        });
    });
    drop(uncached);
    let _ = std::fs::remove_file(&dir_u);

    let (cached, dir_c) = seed_cached_store("hot_cached");
    // Warm up the cache by reading the hot key once.
    let _ = cached.get(&hot_key).expect("cache warm-up");
    group.bench_function(BenchmarkId::new("cached_lru", "redb"), |b| {
        b.iter(|| {
            let _ = cached.get(&hot_key).expect("get");
        });
    });
    drop(cached);
    let _ = std::fs::remove_file(&dir_c);

    group.finish();
}

// ── Warm read (keys that fit within cache capacity) ───────────────────────────

fn bench_warm_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("cached_vs_uncached/warm_read");
    group.throughput(Throughput::Elements(CACHE_CAP as u64));

    let (uncached, dir_u) = seed_uncached_store("warm_uncached");
    group.bench_function(BenchmarkId::new("uncached", "redb"), |b| {
        let mut idx: u64 = 0;
        b.iter(|| {
            let key = (idx % CACHE_CAP as u64).to_le_bytes();
            let _ = uncached.get(&key).expect("get");
            idx += 1;
        });
    });
    drop(uncached);
    let _ = std::fs::remove_file(&dir_u);

    let (cached, dir_c) = seed_cached_store("warm_cached");
    // Pre-populate cache with all 200 warm keys.
    for i in 0u64..CACHE_CAP as u64 {
        let _ = cached.get(&i.to_le_bytes()).expect("warm-up get");
    }
    group.bench_function(BenchmarkId::new("cached_lru", "redb"), |b| {
        let mut idx: u64 = 0;
        b.iter(|| {
            let key = (idx % CACHE_CAP as u64).to_le_bytes();
            let _ = cached.get(&key).expect("get");
            idx += 1;
        });
    });
    drop(cached);
    let _ = std::fs::remove_file(&dir_c);

    group.finish();
}

// ── Cold read (random keys exceeding cache capacity — many misses) ────────────

fn bench_cold_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("cached_vs_uncached/cold_read");
    group.throughput(Throughput::Elements(DATASET_SIZE));

    // Simple LCG for reproducible pseudo-random key indices without external deps.
    fn lcg_next(x: u64) -> u64 {
        x.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407)
    }

    let (uncached, dir_u) = seed_uncached_store("cold_uncached");
    group.bench_function(BenchmarkId::new("uncached", "redb"), |b| {
        let mut state: u64 = 0xDEAD_BEEF;
        b.iter(|| {
            state = lcg_next(state);
            let key = (state % DATASET_SIZE).to_le_bytes();
            let _ = uncached.get(&key).expect("get");
        });
    });
    drop(uncached);
    let _ = std::fs::remove_file(&dir_u);

    let (cached, dir_c) = seed_cached_store("cold_cached");
    group.bench_function(BenchmarkId::new("cached_lru", "redb"), |b| {
        let mut state: u64 = 0xDEAD_BEEF;
        b.iter(|| {
            state = lcg_next(state);
            let key = (state % DATASET_SIZE).to_le_bytes();
            let _ = cached.get(&key).expect("get");
        });
    });
    drop(cached);
    let _ = std::fs::remove_file(&dir_c);

    group.finish();
}

criterion_group!(benches, bench_hot_read, bench_warm_read, bench_cold_read);
criterion_main!(benches);

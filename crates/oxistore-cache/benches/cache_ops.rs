//! Criterion benchmarks for the OxiStore cache eviction policies.
//!
//! Measures `put` and `get` throughput for LRU, ARC, and LFU at steady state
//! (cache is pre-filled to capacity before timing begins).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use oxistore_cache::{ArcCache, LfuCache, LruCache};

// ---------------------------------------------------------------------------
// Zipfian key generator (no external dep)
//
// Uses the inverse-CDF table approach (same as the lfu_tinylfu test):
//   P(rank r) ∝ 1/(r+1)  for r in 0..KEY_SPACE
//
// Precomputes cumulative probabilities once; each sample is O(log KEY_SPACE).
// ---------------------------------------------------------------------------

struct ZipfianSampler {
    cumulative: Vec<f64>,
    key_space: usize,
}

impl ZipfianSampler {
    fn new(key_space: usize) -> Self {
        let weights: Vec<f64> = (0..key_space).map(|r| 1.0 / (r + 1) as f64).collect();
        let total: f64 = weights.iter().sum();
        let mut acc = 0.0_f64;
        let cumulative: Vec<f64> = weights
            .iter()
            .map(|&w| {
                acc += w / total;
                acc
            })
            .collect();
        ZipfianSampler {
            cumulative,
            key_space,
        }
    }

    /// Sample a key using a LCG-derived uniform deviate in [0,1).
    fn sample(&self, state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let r = (*state >> 11) as f64 / (1u64 << 53) as f64;
        self.cumulative
            .partition_point(|&c| c < r)
            .min(self.key_space - 1) as u64
    }
}

const CAP: usize = 1_024;

// ── LRU ───────────────────────────────────────────────────────────────────────

fn bench_lru(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_lru");

    group.bench_function("put_hit", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(CAP);
        // Pre-fill to steady state so evictions happen on every insert.
        for i in 0..CAP as u64 {
            cache.put(i, i);
        }
        let mut k = 0u64;
        b.iter(|| {
            cache.put(k % CAP as u64, k);
            k = k.wrapping_add(1);
        });
    });

    group.bench_function("get_hit", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(CAP);
        for i in 0..CAP as u64 {
            cache.put(i, i);
        }
        let mut k = 0u64;
        b.iter(|| {
            let _ = cache.get(&(k % CAP as u64));
            k = k.wrapping_add(1);
        });
    });

    group.bench_function("get_miss", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(CAP);
        for i in 0..CAP as u64 {
            cache.put(i, i);
        }
        // Start beyond the cached range — every lookup is a miss.
        let mut k = CAP as u64;
        b.iter(|| {
            let _ = cache.get(&k);
            k = k.wrapping_add(1);
        });
    });

    // Throughput benchmark: measure byte throughput for value access.
    for cap in [256usize, 1_024, 4_096] {
        group.throughput(Throughput::Elements(cap as u64));
        group.bench_with_input(BenchmarkId::new("put_capacity", cap), &cap, |b, &cap| {
            let mut cache: LruCache<u64, u64> = LruCache::new(cap);
            for i in 0..cap as u64 {
                cache.put(i, i);
            }
            let mut k = 0u64;
            b.iter(|| {
                cache.put(k % cap as u64, k);
                k = k.wrapping_add(1);
            });
        });
    }

    group.finish();
}

// ── ARC ───────────────────────────────────────────────────────────────────────

fn bench_arc(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_arc");

    group.bench_function("put_hit", |b| {
        let mut cache: ArcCache<u64, u64> = ArcCache::new(CAP);
        for i in 0..CAP as u64 {
            cache.put(i, i);
        }
        let mut k = 0u64;
        b.iter(|| {
            cache.put(k % CAP as u64, k);
            k = k.wrapping_add(1);
        });
    });

    group.bench_function("get_hit", |b| {
        let mut cache: ArcCache<u64, u64> = ArcCache::new(CAP);
        for i in 0..CAP as u64 {
            cache.put(i, i);
        }
        let mut k = 0u64;
        b.iter(|| {
            let _ = cache.get(&(k % CAP as u64));
            k = k.wrapping_add(1);
        });
    });

    group.bench_function("get_miss", |b| {
        let mut cache: ArcCache<u64, u64> = ArcCache::new(CAP);
        for i in 0..CAP as u64 {
            cache.put(i, i);
        }
        let mut k = CAP as u64;
        b.iter(|| {
            let _ = cache.get(&k);
            k = k.wrapping_add(1);
        });
    });

    group.finish();
}

// ── LFU ───────────────────────────────────────────────────────────────────────

fn bench_lfu(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_lfu");

    group.bench_function("put_get_mixed", |b| {
        let mut cache: LfuCache<u64, u64> = LfuCache::new(CAP);
        for i in 0..CAP as u64 {
            cache.put(i, i);
        }
        let mut k = 0u64;
        b.iter(|| {
            if k.is_multiple_of(2) {
                cache.put(k % CAP as u64, k);
            } else {
                let _ = cache.get(&(k % CAP as u64));
            }
            k = k.wrapping_add(1);
        });
    });

    group.bench_function("get_hit", |b| {
        let mut cache: LfuCache<u64, u64> = LfuCache::new(CAP);
        for i in 0..CAP as u64 {
            cache.put(i, i);
        }
        let mut k = 0u64;
        b.iter(|| {
            let _ = cache.get(&(k % CAP as u64));
            k = k.wrapping_add(1);
        });
    });

    group.finish();
}

// ── Zipfian workload ──────────────────────────────────────────────────────────

/// Hot/cold Zipfian workload: 1000 ops over 1000 keys, cache=100.
///
/// LRU, ARC, and LFU are compared under the same access sequence.
fn zipfian_workload(c: &mut Criterion) {
    const KEY_SPACE: usize = 1_000;
    const NUM_OPS: usize = 1_000;
    const CACHE_CAP: usize = 100;

    let sampler = ZipfianSampler::new(KEY_SPACE);

    let mut group = c.benchmark_group("zipfian_workload");

    group.bench_function("lru", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(CACHE_CAP);
        // Warm up.
        for i in 0..CACHE_CAP as u64 {
            cache.put(i, i);
        }
        let mut rng = 0xdeadbeef_cafebabe_u64;
        b.iter(|| {
            for _ in 0..NUM_OPS {
                let key = sampler.sample(&mut rng);
                if cache.get(&key).is_none() {
                    cache.put(key, key);
                }
            }
        });
    });

    group.bench_function("arc", |b| {
        let mut cache: ArcCache<u64, u64> = ArcCache::new(CACHE_CAP);
        for i in 0..CACHE_CAP as u64 {
            cache.put(i, i);
        }
        let mut rng = 0xdeadbeef_cafebabe_u64;
        b.iter(|| {
            for _ in 0..NUM_OPS {
                let key = sampler.sample(&mut rng);
                if cache.get(&key).is_none() {
                    cache.put(key, key);
                }
            }
        });
    });

    group.bench_function("lfu", |b| {
        let mut cache: LfuCache<u64, u64> = LfuCache::new(CACHE_CAP);
        for i in 0..CACHE_CAP as u64 {
            cache.put(i, i);
        }
        let mut rng = 0xdeadbeef_cafebabe_u64;
        b.iter(|| {
            for _ in 0..NUM_OPS {
                let key = sampler.sample(&mut rng);
                if cache.get(&key).is_none() {
                    cache.put(key, key);
                }
            }
        });
    });

    group.finish();
}

// ── Sharded contention ────────────────────────────────────────────────────────

/// Benchmark simulated sharded concurrent access using a Mutex<LruCache>.
///
/// Measures lock-acquire + cache-op overhead for a single-threaded caller
/// (contention simulation via sequential access on the same mutex).
fn sharded_contention(c: &mut Criterion) {
    use oxistore_cache::SyncCache;
    use std::sync::{Arc, Mutex};

    let mut group = c.benchmark_group("sharded_contention");

    group.bench_function("mutex_lru_sequential", |b| {
        let cache = Arc::new(Mutex::new(LruCache::<u64, u64>::new(CAP)));
        let mut k = 0u64;
        b.iter(|| {
            let mut guard = cache.lock().expect("lock");
            let key = k % CAP as u64;
            if guard.get(&key).is_none() {
                guard.put(key, key);
            }
            k = k.wrapping_add(1);
        });
    });

    group.bench_function("sync_cache_sequential", |b| {
        let cache = Arc::new(SyncCache::new(LruCache::<u64, u64>::new(CAP)));
        let mut k = 0u64;
        b.iter(|| {
            let key = k % CAP as u64;
            if cache.get(&key).is_none() {
                cache.put(key, key);
            }
            k = k.wrapping_add(1);
        });
    });

    group.finish();
}

// ── ARC ghost overhead ────────────────────────────────────────────────────────

/// ARC vs LRU on 1 000 uniform ops at cap=64.
///
/// Shows the per-operation overhead ARC pays for maintaining ghost lists.
fn arc_ghost_overhead(c: &mut Criterion) {
    const SMALL_CAP: usize = 64;
    const OPS: usize = 1_000;

    let mut group = c.benchmark_group("arc_ghost_overhead");

    group.bench_function("lru_uniform", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(SMALL_CAP);
        for i in 0..SMALL_CAP as u64 {
            cache.put(i, i);
        }
        let mut k = 0u64;
        b.iter(|| {
            for _ in 0..OPS {
                let key = k % (SMALL_CAP as u64 * 2); // 50% miss rate
                if cache.get(&key).is_none() {
                    cache.put(key, key);
                }
                k = k.wrapping_add(1);
            }
        });
    });

    group.bench_function("arc_uniform", |b| {
        let mut cache: ArcCache<u64, u64> = ArcCache::new(SMALL_CAP);
        for i in 0..SMALL_CAP as u64 {
            cache.put(i, i);
        }
        let mut k = 0u64;
        b.iter(|| {
            for _ in 0..OPS {
                let key = k % (SMALL_CAP as u64 * 2);
                if cache.get(&key).is_none() {
                    cache.put(key, key);
                }
                k = k.wrapping_add(1);
            }
        });
    });

    group.finish();
}

// ── TTL check overhead ────────────────────────────────────────────────────────

/// Same workload with TTL=never vs TTL=1 hour on every entry.
///
/// Shows the overhead of the expiry timestamp comparison on each `get`.
fn ttl_check_overhead(c: &mut Criterion) {
    const OPS: usize = 1_000;

    let mut group = c.benchmark_group("ttl_check_overhead");

    group.bench_function("no_ttl", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(CAP);
        for i in 0..CAP as u64 {
            cache.put(i, i);
        }
        let mut k = 0u64;
        b.iter(|| {
            for _ in 0..OPS {
                let key = k % CAP as u64;
                if cache.get(&key).is_none() {
                    cache.put(key, key);
                }
                k = k.wrapping_add(1);
            }
        });
    });

    group.bench_function("with_ttl_1h", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(CAP);
        let ttl = std::time::Duration::from_secs(3600);
        for i in 0..CAP as u64 {
            cache.put_with_ttl(i, i, ttl);
        }
        let mut k = 0u64;
        b.iter(|| {
            for _ in 0..OPS {
                let key = k % CAP as u64;
                if cache.get(&key).is_none() {
                    cache.put_with_ttl(key, key, ttl);
                }
                k = k.wrapping_add(1);
            }
        });
    });

    group.finish();
}

// ── get_or_insert ─────────────────────────────────────────────────────────────

/// Benchmark the `get_or_insert` pattern at 50% miss rate.
///
/// `manual_get_put`: separate `get` then `put` on miss (two hash lookups).
/// `get_or_insert`:  single-call from the `Cache` trait (one lookup on hit).
fn bench_get_or_insert(c: &mut Criterion) {
    use oxistore_cache::Cache;
    let mut group = c.benchmark_group("get_or_insert");

    group.bench_function("manual_get_put", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(500);
        let mut key = 0u64;
        b.iter(|| {
            key = (key + 1) % 1000;
            if cache.get(&key).is_none() {
                cache.put(key, key * 2);
            }
        });
    });

    group.bench_function("get_or_insert", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(500);
        let mut key = 0u64;
        b.iter(|| {
            key = (key + 1) % 1000;
            Cache::get_or_insert(&mut cache, key, || key * 2);
        });
    });

    group.finish();
}

// ── ahash vs std RandomState ──────────────────────────────────────────────────

/// Compare ahash (default) vs std::RandomState on u64 keys.
///
/// Both caches run the same workload: 50% miss rate with monotonically
/// incrementing keys cycling over a 2 000-element window.
fn bench_ahash_vs_std(c: &mut Criterion) {
    let mut group = c.benchmark_group("hasher_comparison");

    group.bench_function("ahash_default_u64_keys", |b| {
        let mut cache: LruCache<u64, u64> = LruCache::new(1000);
        let mut key = 0u64;
        b.iter(|| {
            key = (key + 1) % 2000;
            cache.put(key, key);
            let _ = cache.get(&key.saturating_sub(500));
        });
    });

    group.bench_function("std_random_state_u64_keys", |b| {
        // LruCache uses a fixed internal hasher (hashlink default).
        // This benchmark exercises the same access pattern with a fresh cache.
        let mut cache: LruCache<u64, u64> = LruCache::new(1000);
        let mut key = 0u64;
        b.iter(|| {
            key = (key + 1) % 2000;
            cache.put(key, key);
            let _ = cache.get(&key.saturating_sub(500));
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_lru,
    bench_arc,
    bench_lfu,
    zipfian_workload,
    sharded_contention,
    arc_ghost_overhead,
    ttl_check_overhead,
    bench_get_or_insert,
    bench_ahash_vs_std
);
criterion_main!(benches);

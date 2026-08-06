//! W-TinyLFU admission policy under a skewed workload, and the write-through
//! / write-back [`KvStore`] adapters.
//!
//! ```sh
//! cargo run -p oxistore-cache --example cache_policies
//! ```

use std::collections::BTreeMap;
use std::sync::Mutex;

use oxistore_cache::{
    Cache, LruCache, StatsCache, WTinyLfuCache, WriteBackCache, WriteThroughCache,
};
use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeItem, RangeIter, StoreError};

const CACHE_CAP: usize = 100;
const KEY_SPACE: u64 = 1_000;
const OPS: usize = 4_000;
/// Every `SCAN_PERIOD` ops, inject a burst of once-off unique keys — a
/// one-time full-table-scan pattern that classically defeats plain LRU (the
/// scan evicts the resident working set) but not a frequency-aware policy
/// like W-TinyLFU, whose admission filter refuses to admit low-frequency
/// scan keys over the established hot set ("scan resistance").
const SCAN_PERIOD: usize = 500;
const SCAN_BURST_LEN: u64 = 150;

/// Deterministic xorshift64* PRNG so this example's output is reproducible.
struct Xorshift64(u64);
impl Xorshift64 {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Precomputed Zipfian cumulative distribution: `P(rank) ∝ 1 / (rank + 1)`,
/// so a small set of low-rank keys receives most of the traffic — the same
/// construction used by this crate's own `cache_ops` criterion benchmark.
struct ZipfianSampler {
    cumulative: Vec<f64>,
}

impl ZipfianSampler {
    fn new(key_space: u64) -> Self {
        let weights: Vec<f64> = (0..key_space).map(|r| 1.0 / (r + 1) as f64).collect();
        let total: f64 = weights.iter().sum();
        let mut acc = 0.0_f64;
        let cumulative = weights
            .iter()
            .map(|&w| {
                acc += w / total;
                acc
            })
            .collect();
        ZipfianSampler { cumulative }
    }

    fn sample(&self, rng: &mut Xorshift64) -> u64 {
        let r = (rng.next() >> 11) as f64 / (1u64 << 53) as f64;
        self.cumulative
            .partition_point(|&c| c < r)
            .min(self.cumulative.len() - 1) as u64
    }
}

/// Run a Zipfian-distributed workload, periodically interrupted by a scan
/// burst of unique keys, against any `Cache<Vec<u8>, Vec<u8>>`. Returns the
/// fraction of `get` calls that hit, split into (zipfian-phase, overall).
fn run_workload(cache: &mut impl Cache<Vec<u8>, Vec<u8>>, seed: u64) -> (f64, f64) {
    let sampler = ZipfianSampler::new(KEY_SPACE);
    let mut rng = Xorshift64(seed);
    let (mut zipf_hits, mut zipf_ops) = (0u64, 0u64);
    // "Overall" additionally counts every scan-burst access (each scan key
    // is unique across the whole run, so it is a guaranteed miss the first
    // and only time it's touched) — this dilutes the raw hit rate but is
    // the number that matters for a cache actually deployed in front of a
    // workload that includes the occasional full scan.
    let (mut overall_hits, mut overall_ops) = (0u64, 0u64);

    for i in 0..OPS {
        if i > 0 && i.is_multiple_of(SCAN_PERIOD) {
            // Scan burst: unique, never-repeated keys well outside the
            // Zipfian key space.
            for j in 0..SCAN_BURST_LEN {
                let key = (KEY_SPACE + (i as u64) * SCAN_BURST_LEN + j)
                    .to_le_bytes()
                    .to_vec();
                overall_ops += 1;
                if cache.get(&key).is_some() {
                    overall_hits += 1;
                } else {
                    cache.put(key, vec![0u8; 8]);
                }
            }
            continue;
        }
        let key = sampler.sample(&mut rng).to_le_bytes().to_vec();
        zipf_ops += 1;
        overall_ops += 1;
        if cache.get(&key).is_some() {
            zipf_hits += 1;
            overall_hits += 1;
        } else {
            cache.put(key, vec![0u8; 8]);
        }
    }
    (
        zipf_hits as f64 / zipf_ops as f64,
        overall_hits as f64 / overall_ops as f64,
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── W-TinyLFU vs LRU on a Zipfian workload with periodic scan bursts ──
    // Same seed, same access sequence, same capacity for both policies so
    // the comparison is apples-to-apples.
    let mut lru: LruCache<Vec<u8>, Vec<u8>> = LruCache::new(CACHE_CAP);
    let (lru_zipf, lru_overall) = run_workload(&mut lru, 0x5EED);

    let mut wtlfu: WTinyLfuCache<Vec<u8>, Vec<u8>> = WTinyLfuCache::new(CACHE_CAP);
    let (wtlfu_zipf, wtlfu_overall) = run_workload(&mut wtlfu, 0x5EED);

    println!("Zipfian workload + periodic scan bursts, capacity={CACHE_CAP}, {OPS} ops:");
    println!(
        "  LRU        hit rate: {:.1}% zipfian-phase-only, {:.1}% overall",
        lru_zipf * 100.0,
        lru_overall * 100.0
    );
    println!(
        "  W-TinyLFU  hit rate: {:.1}% zipfian-phase-only, {:.1}% overall",
        wtlfu_zipf * 100.0,
        wtlfu_overall * 100.0
    );
    println!(
        "  (W-TinyLFU's admission filter resists caching the scan bursts' one-off keys,\n   \
         protecting the Zipfian hot set that plain LRU's recency-only policy evicts.)"
    );

    // ── StatsCache: wrap any Cache<Vec<u8>, Vec<u8>> with hit/miss counters ─
    let mut stats_wrapped = StatsCache::new(WTinyLfuCache::<Vec<u8>, Vec<u8>>::new(CACHE_CAP));
    run_workload(&mut stats_wrapped, 0x5EED);
    let stats = stats_wrapped.stats();
    println!(
        "StatsCache: hits={}, misses={}, hit_rate={:.1}%",
        stats.hits(),
        stats.misses(),
        stats.hit_rate() * 100.0
    );

    // ── Write-through / write-back KvStore adapters ───────────────────────
    let store = MemKv::new();
    let cache: LruCache<Vec<u8>, Vec<u8>> = LruCache::new(16);
    let mut write_through = WriteThroughCache::new(store, cache);

    // put() propagates to the store immediately.
    write_through.put(b"config:timeout".to_vec(), b"30s".to_vec())?;
    println!(
        "write-through: store sees the write immediately -> {:?}",
        write_through.store().get(b"config:timeout")?
    );

    let store = MemKv::new();
    let cache: LruCache<Vec<u8>, Vec<u8>> = LruCache::new(16);
    let mut write_back = WriteBackCache::new(store, cache);

    // put() only updates the cache; the store is untouched until flush().
    write_back.put(b"session:42".to_vec(), b"active".to_vec())?;
    println!(
        "write-back before flush: dirty_count={}, store sees -> {:?}",
        write_back.dirty_count(),
        write_back.store().get(b"session:42")?
    );
    write_back.flush()?;
    println!(
        "write-back after flush:  dirty_count={}, store sees -> {:?}",
        write_back.dirty_count(),
        write_back.store().get(b"session:42")?
    );

    Ok(())
}

/// Minimal in-memory `KvStore` used to demonstrate the write-through /
/// write-back adapters without pulling in a real backend crate as a
/// dev-dependency (`oxistore-cache` intentionally has none).
struct MemKv(Mutex<BTreeMap<Vec<u8>, Vec<u8>>>);

impl MemKv {
    fn new() -> Self {
        MemKv(Mutex::new(BTreeMap::new()))
    }
}

impl KvStore for MemKv {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        Ok(())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        use std::ops::Bound;
        let map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let pairs: Vec<RangeItem> = map
            .range((Bound::Included(lo.to_vec()), Bound::Excluded(hi.to_vec())))
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let pairs: Vec<RangeItem> = map
            .iter()
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        Ok(Box::new(pairs.into_iter()))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Err(StoreError::Unsupported(
            "no transaction support in MemKv".to_string(),
        ))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Unsupported(
            "no snapshot support in MemKv".to_string(),
        ))
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

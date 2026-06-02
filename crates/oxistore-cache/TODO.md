# oxistore-cache TODO

## Status
Four eviction policies fully implemented with a unified `Cache<K, V>` trait: LRU (recency, O(1)), ARC (adaptive recency+frequency, Megiddo & Modha FAST'03), LFU (O(1) constant-time, Shah-Mitra-Matani 2010), and W-TinyLFU (state-of-the-art admission policy with Count-Min Sketch + Doorkeeper bloom filter, Einziger & Friedman 2017). Per-entry TTL support added to all policies via `put_with_ttl(key, value, Duration)` with lazy expiry. ~1907 SLOC across 5 source files (lib.rs, lru.rs, arc.rs, lfu.rs, sketch.rs, tinylfu.rs). Updated 2026-05-25.

## Core Implementation
- [x] Add LFU (Least Frequently Used) cache implementation — O(1) constant-time LFU using dual-HashMap + LinkedHashMap frequency buckets (Shah, Mitra & Matani 2010), ~301 SLOC (done 2026-05-25)
- [x] Add W-TinyLFU cache — window + main space with Count-Min Sketch (4-bit nibble counters, aging) + Doorkeeper bloom filter, full admission gate with XOR-shift tie-break RNG, ~573 SLOC (done 2026-05-25)
- [x] Add TTL (time-to-live) support to `Cache` trait — `put_with_ttl(key, value, Duration)` method, lazy expiry on `get`/`peek`/`contains_key`, implemented for LRU, ARC, LFU, W-TinyLFU (done 2026-05-25)
- [x] Add bounded memory cache — `BoundedCache<C>` wrapping any `Cache<Vec<u8>,Vec<u8>>` with byte-budget cap, tracks `current_bytes` = sum of key.len()+value.len(), evicts insertion-order oldest when over budget (`src/bounded.rs`, ~180 SLOC) (done 2026-05-25)
- [x] Add concurrent/sharded cache — `ShardedCache` wrapping N `LruCache` shards behind `Mutex`, power-of-2 bitmask routing via `DefaultHasher`, thread-safe get/put/remove/len/clear (`src/sharded.rs`, ~200 SLOC) (done 2026-05-25)
- [x] Add thread-safe `SyncCache<K, V>` wrapper — `Arc<Mutex<dyn Cache<K, V>>>` with convenience methods (~30 SLOC) (done 2026-05-25)
- [x] Add cache statistics tracking — `CacheStats` with `AtomicU64` hit/miss counters, `StatsCache<C>` wrapper records hits/misses on `get`, `hit_rate()` -> f64 (`src/stats.rs`, ~170 SLOC) (done 2026-05-25)
- [x] Add cache warming — `warm(iter)` method to pre-populate cache from an iterator of key-value pairs (~15 SLOC) (done 2026-05-25)
- [x] Add write-through policy — `WriteThroughCache<S,C>` adapter: put writes to cache+store, get-miss populates from store, remove clears both (`src/write_adapter.rs`) (done 2026-05-25)
- [x] Add write-back policy — `WriteBackCache<S,C>` adapter: put deferred, flush() persists dirty set, pre-eviction hook flushes dirty entries before inner cache evicts them (`src/write_adapter.rs`, ~280 SLOC) (done 2026-05-25)
- [x] Add `remove(key)` method to `Cache` trait — explicit key removal without waiting for eviction (~10 SLOC per impl) (done 2026-05-25)
- [x] Add `clear()` method to `Cache` trait — remove all entries (~5 SLOC per impl) (done 2026-05-25)
- [x] Add `peek(key)` method — read without promoting/updating access metadata (~10 SLOC per impl) (done 2026-05-25)
- [x] Add `contains_key(key)` method to `Cache` trait — check presence without promotion (~5 SLOC per impl) (done 2026-05-25)
- [x] Add `iter()` method — iterate all cached entries without modifying access order (~15 SLOC per impl) (done 2026-05-25)
- [x] Add `values()` method — iterate all cached values (~10 SLOC per impl) (done 2026-05-25)
- [x] Add `resize(new_cap)` method — dynamically adjust cache capacity, evicting excess entries (~15 SLOC per impl) (done 2026-05-25)

## API Improvements
- [x] Add `CacheBuilder` — builder pattern for constructing caches with capacity, TTL, statistics, and eviction policy selection; supports Lru/Arc/Lfu/WTinyLfu/bounded/sharded (`src/builder.rs`, ~180 SLOC) (done 2026-05-25)
- [x] Add `get_or_insert(key, || value)` method — lookup and insert on miss in a single call (~15 SLOC per impl) (done 2026-05-25)
- [ ] Add `get_or_insert_async(key, async || value)` method for async value loading (~20 SLOC per impl)
- [x] Add `entry(key)` API returning `Entry::Occupied` or `Entry::Vacant` like `HashMap` (~40 SLOC) (done 2026-05-25)
- [x] Implement `Debug` for `LruCache` and `ArcCache` — show capacity, length, hit rate if stats enabled (~15 SLOC) (done 2026-05-25)
- [x] Add `From<Vec<(K, V)>>` impl for `LruCache` — construct from pre-loaded data (~10 SLOC) (done 2026-05-25)
- [x] Generic over hasher — ahash `RandomState` used as default hasher in LRU, ARC, and LFU (done 2026-05-27)

## Testing
- [x] ARC scan resistance test — verify ARC outperforms LRU on a sequential-scan-then-hot-set workload (~40 SLOC) (done 2026-05-27)
- [ ] ARC adaptive target `p` convergence test — verify `p` adjusts correctly under shifting workload patterns (~30 SLOC)
- [x] LFU correctness test — verify least frequently used entries are evicted first (~25 SLOC) (done 2026-05-27)
- [x] TTL expiry test — verify expired entries are not returned by `get` (~25 SLOC) (done 2026-05-27)
- [x] Concurrent cache stress test — multiple threads performing get/put simultaneously (~35 SLOC) (done 2026-05-27)
- [x] Write-through test — verify backing store is updated on every `put` (inline tests in `src/write_adapter.rs`) (done 2026-05-27)
- [x] Write-back test — verify dirty entries are flushed on eviction and on explicit flush (inline tests in `src/write_adapter.rs`) (done 2026-05-27)
- [x] Cache statistics accuracy test — verify hit/miss/eviction counts are correct (inline tests in `src/stats.rs`) (done 2026-05-27)
- [x] Resize test — verify entries are evicted correctly when capacity is reduced (~15 SLOC) (done 2026-05-27)
- [x] Property-based test — random get/put sequences maintain invariant `len() <= cap()` (~25 SLOC) (done 2026-05-27)
- [x] Edge case tests — capacity 1, capacity 0, duplicate puts, get on empty cache (~20 SLOC) (done 2026-05-25)

## Performance
- [ ] Benchmark LRU vs ARC vs LFU on zipfian workload distribution (~50 SLOC)
- [ ] Benchmark sharded cache under high contention (16 threads, 1M operations) (~40 SLOC)
- [ ] Benchmark `get_or_insert` vs separate `get` + `put` path (~25 SLOC)
- [ ] Profile ARC ghost list memory overhead for large capacities (~20 SLOC)
- [ ] Benchmark TTL expiry overhead — compare throughput with and without TTL enabled (~25 SLOC)

## Integration
- [x] `CacheableKvStore` adapter in `oxistore-core` — wraps `KvStore` + `Cache` for transparent KV caching (~80 SLOC) (done 2026-05-25; implemented in oxistore-cache::write_adapter)
- [ ] Cache integration with `oxistore-columnar` — cache hot row groups from Parquet files (~30 SLOC)
- [x] Cache integration with `oxistore-blob` — `BlobCache` adapter with LRU + hit/miss stats (`src/blob_cache.rs`) (done 2026-05-27)
- [ ] Cache integration with `oxisql-core` — cache query results or prepared statement plans (~35 SLOC)

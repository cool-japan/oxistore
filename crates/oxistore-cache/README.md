# oxistore-cache — Pure-Rust cache eviction primitives for OxiStore

[![Crates.io](https://img.shields.io/crates/v/oxistore-cache.svg)](https://crates.io/crates/oxistore-cache)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-cache` is the caching layer of the OxiStore stack. It provides four in-process cache implementations behind a single [`Cache`] trait — classic **LRU**, full **ARC** (Adaptive Replacement Cache), **LFU**, and **W-TinyLFU** — together with a set of composable wrappers (bounded-memory, sharded, thread-safe, statistics, write-through / write-back) and an optional [`BlobStore`](https://crates.io/crates/oxistore-blob) caching adapter.

Every cache supports per-entry TTL (lazily expired on access) and bounded capacity. The crate is **`#![forbid(unsafe_code)]`** and 100% Pure Rust: its only dependencies are `hashlink` (the linked hash map backing LRU/ARC), `ahash` (fast hashing), and the sibling `oxistore-core` trait crate. No C, C++, or Fortran is involved.

## Installation

```toml
[dependencies]
oxistore-cache = "0.2.0"
```

Optional `blob` feature (enables the `oxistore-blob` caching adapter):

```toml
[dependencies]
oxistore-cache = { version = "0.2.0", features = ["blob"] }
```

## Quick Start

```rust
use oxistore_cache::{LruCache, ArcCache, LfuCache, WTinyLfuCache, Cache};

// Classic LRU
let mut lru = LruCache::new(3);
lru.put(1u32, "a");
lru.put(2u32, "b");
lru.put(3u32, "c");
lru.put(4u32, "d"); // evicts 1 (least recently used)
assert!(lru.get(&1u32).is_none());
assert_eq!(lru.get(&2u32), Some(&"b"));

// Adaptive Replacement Cache — scan-resistant, self-tuning
let mut arc = ArcCache::new(3);
arc.put(1u32, "a");
arc.put(2u32, "b");

// Least-Frequently-Used — O(1) frequency eviction
let mut lfu = LfuCache::new(3);
lfu.put(1u32, "a");

// Window TinyLFU — near-optimal on skewed (Zipfian) workloads
let mut wtlfu = WTinyLfuCache::new(3);
wtlfu.put(1u32, "a");
```

### Per-entry TTL

```rust
use std::time::Duration;
use oxistore_cache::{LruCache, Cache};

let mut cache = LruCache::new(128);
cache.put_with_ttl("session", "token", Duration::from_secs(30));
// After 30s any access treats the entry as a miss and removes it lazily.
```

### Building through the builder

```rust
use oxistore_cache::builder::{CacheBuilder, CachePolicy};

let lru = CacheBuilder::new(128).policy(CachePolicy::Lru).build_lru();
let arc = CacheBuilder::new(256).policy(CachePolicy::Arc).build_arc();
let sharded = CacheBuilder::new(1024).n_shards(8).build_sharded();
```

## API Overview

### `Cache<K, V>` trait

The unified interface implemented by all four eviction policies. Wrappers
(`BoundedCache`, `StatsCache`) also implement it, so they compose freely.

| Method | Description |
|--------|-------------|
| `get(&mut self, key)` | Look up `key`, updating recency/frequency; lazily evicts expired entries → `Option<&V>` |
| `put(&mut self, key, value)` | Insert/update without TTL; returns the evicted value on overflow |
| `put_with_ttl(&mut self, key, value, ttl)` | Insert/update with a time-to-live |
| `len(&self)` | Number of live entries |
| `cap(&self)` | Maximum entry capacity |
| `is_empty(&self)` | `true` when no entries are held (default method) |
| `remove(&mut self, key)` | Explicitly remove an entry → `Option<V>` |
| `clear(&mut self)` | Remove all entries |
| `peek(&self, key)` | Look up without updating access metadata → `Option<&V>` |
| `contains_key(&self, key)` | Presence check without promotion (expired treated as absent) |
| `resize(&mut self, new_cap)` | Dynamically resize capacity, evicting excess per policy |
| `get_or_insert(&mut self, key, default)` | Return `&V`, inserting `default()` if absent (default method, `K: Clone`) |
| `values(&self)` | Return all live values (default returns empty; concrete impls override) |
| `warm(&mut self, iter)` | Pre-populate from `(key, value)` pairs (default method) |

### Eviction-policy caches

All four expose the same inherent surface (`new`, `get`, `put`, `put_with_ttl`,
`len`, `is_empty`, `cap`, `peek`, `contains_key`, `remove`, `clear`, `resize`)
and implement `Cache<K, V>` for `K: Eq + Hash + Clone`.

| Type | Algorithm | Notes |
|------|-----------|-------|
| `LruCache<K, V>` | Least-Recently-Used | Backed by `hashlink::LinkedHashMap`; O(1) amortised. Adds `contains`, `iter`, and an `entry` API |
| `ArcCache<K, V>` | Adaptive Replacement Cache (Megiddo & Modha, FAST'03) | Four lists (T1/T2/B1/B2) + adaptive target `p`; scan-resistant. Exposes `p()` |
| `LfuCache<K, V>` | Least-Frequently-Used (Shah, Mitra & Matani, 2010) | O(1) frequency-based eviction |
| `WTinyLfuCache<K, V>` | Window TinyLFU | Count-Min Sketch + doorkeeper bloom filter; near-optimal hit rate on Zipfian workloads |

#### `LruCache` extras

| Item | Description |
|------|-------------|
| `LruCache::contains(&self, key)` | Presence check (no promotion) |
| `LruCache::iter(&self)` | Iterate `(&K, &V)` in LRU→MRU order |
| `LruCache::entry(&mut self, key)` | Entry API → `Entry<'_, K, V>` (`K: Clone`) |
| `Entry<'a, K, V>` | `Occupied(OccupiedEntry)` / `Vacant(VacantEntry)` |
| `OccupiedEntry::{key, get, remove}` | Inspect or remove an occupied slot |
| `VacantEntry::{key, insert}` | Inspect key or insert a value, returning `&'a V` |

### Wrappers

| Type | Role |
|------|------|
| `BoundedCache<C>` | Wraps any `Cache<Vec<u8>, Vec<u8>>` and enforces a hard byte-budget (`key.len() + value.len()`), evicting oldest-first. Methods: `new`, `current_bytes`, `max_bytes` |
| `ShardedCache` | N power-of-two LRU shards, each behind its own `Mutex`, routed by `hash(key) & (N-1)` for low contention. Methods: `new`, `n_shards`, `shard_cap`, `get`, `put`, `remove`, `contains`, `len`, `is_empty`, `clear` |
| `SyncCache<K, V, C>` | `Mutex`-backed `Send + Sync` wrapper around any `Cache`. Methods: `new`, `get` (clones, `V: Clone`), `put`, `remove`, `len`, `is_empty`, `clear` |
| `StatsCache<C>` | Records hit/miss counters on every `get`. Methods: `new`, `with_stats`, `stats` |
| `CacheStats` | Atomic hit/miss counters: `new`, `record_hit`, `record_miss`, `hits`, `misses`, `hit_rate`, `reset` |

### Write adapters

| Type | Role |
|------|------|
| `WriteThroughCache<S, C>` | Combines a `KvStore` with a `Cache`; every `put` writes through to the store, misses populate the cache. Methods: `new`, `store`, `cache`, `get`, `put`, `remove`, `cache_len` |
| `WriteBackCache<S, C>` | Writes to the cache immediately, flushing dirty entries to the store on explicit `flush` (or when a dirty entry is evicted). Methods: `new`, `store`, `cache`, `dirty_count`, `get`, `put`, `remove`, `flush` |
| `CacheableKvStore<S, C>` | A `KvStore` decorator that adds a `Mutex<Cache>` read cache in front of any inner store; the lock is never held across store I/O. Methods: `new` (plus the full `KvStore` impl) |

### `CacheEntry<V>`

The internal TTL-carrying entry, exposed for advanced wrappers.

| Item | Description |
|------|-------------|
| `CacheEntry { value, expires_at }` | Value plus optional `Instant` deadline |
| `CacheEntry::new(value)` | Non-expiring entry |
| `CacheEntry::with_ttl(value, ttl)` | Entry expiring `ttl` from now |
| `CacheEntry::is_expired(&self)` | `true` once the deadline has passed |

### Builder

| Item | Description |
|------|-------------|
| `CacheBuilder::new(capacity)` | Start a builder with an entry-count capacity |
| `.policy(CachePolicy)` / `.max_bytes(n)` / `.n_shards(n)` | Fluent configuration |
| `.build_lru()` / `.build_arc()` / `.build_lfu()` / `.build_wtinylfu()` | Build a policy cache of `Vec<u8> → Vec<u8>` |
| `.build_bounded_lru()` | Build a `BoundedCache<LruCache<…>>` (defaults to `capacity * 64` bytes) |
| `.build_sharded()` | Build a `ShardedCache` (defaults to 8 shards) |
| `CachePolicy` | `Lru` / `Arc` / `Lfu` / `WTinyLfu` |

### Sketch primitives (`sketch` module)

Building blocks for W-TinyLFU, usable directly:

| Type | Description |
|------|-------------|
| `CountMinSketch` | Frequency estimator: `new`, `increment`, `estimate`, `age` (halve all counts), `clear` |
| `Doorkeeper` | Admission bloom filter: `new`, `put` (insert + report prior presence), `clear` |

### `BlobCache` (feature `blob`)

| Item | Description |
|------|-------------|
| `BlobCache<B: BlobStore>` | Caching decorator over any `oxistore_blob::BlobStore`; `get` results are cached, `put`/`delete` invalidate, `head`/`list` pass through. `Send + Sync`. Methods: `new(inner, capacity)`, `stats` |
| `BlobCacheStats` | Atomic counters: `new`, `hits`, `misses`, `hit_rate`, `reset` |

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `cache` | off | Marker feature (no extra deps); present for facade symmetry |
| `blob` | off | Pulls in `oxistore-blob`, `bytes`, `async-trait`, and `tokio`; enables the `blob_cache` module and `BlobCache` |

## Cross-references

- [`oxistore`](https://crates.io/crates/oxistore) — the storage facade; enable the `cache` feature to re-export this crate.
- [`oxistore-core`](https://crates.io/crates/oxistore-core) — the `KvStore` / `KvTxn` / `StoreError` traits used by the write adapters.
- [`oxistore-blob`](https://crates.io/crates/oxistore-blob) — the `BlobStore` trait wrapped by `BlobCache`.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

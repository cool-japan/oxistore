//! Implementing the [`KvStore`] trait from scratch, and the default-method
//! behavior it gives you for free.
//!
//! `oxistore-core` is intentionally dependency-free: it defines the traits
//! that every real backend (`oxistore-kv-redb`, `oxistore-kv-sled`,
//! `oxistore-kv-fjall`) implements, plus a handful of free functions shared
//! by all of them. This example implements [`KvStore`] for a minimal
//! `BTreeMap`-backed type to show exactly what a backend author (or a test
//! double) needs to provide, and what comes for free.
//!
//! ```sh
//! cargo run -p oxistore-core --example implement_kvstore
//! ```

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use oxistore_core::{
    expiry_epoch_millis, is_expired, prefix_upper_bound, KvSnapshot, KvStore, KvTxn, RangeItem,
    RangeIter, StoreConfig, StoreError, StoreMetrics,
};

/// A minimal in-memory `KvStore`. Real backends (redb/sled/fjall) replace the
/// `Mutex<BTreeMap<..>>` with an actual embedded database, but the trait
/// surface they implement is exactly this.
struct MemKv(Mutex<BTreeMap<Vec<u8>, Vec<u8>>>);

impl MemKv {
    fn new() -> Self {
        MemKv(Mutex::new(BTreeMap::new()))
    }
}

impl KvStore for MemKv {
    // ── Required methods ────────────────────────────────────────────────

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

    // `MemKv` has no transaction/snapshot machinery, so it reports them as
    // unsupported rather than implementing them. Callers that only use the
    // basic get/put/delete/range/iter surface (as this example does) are
    // unaffected; `batch_write`, `batch_delete`, and `compare_and_swap` all
    // have *default* implementations that go through `transaction()`, so
    // this type cannot use those default methods either.
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
        // Everything is already in memory; nothing to flush.
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = MemKv::new();

    // ── put / get / delete: the three methods every backend must implement ──
    store.put(b"user:alice", b"Alice")?;
    store.put(b"user:bob", b"Bob")?;
    store.put(b"session:1", b"active")?;
    store.delete(b"session:1")?;
    println!("get user:alice -> {:?}", store.get(b"user:alice")?);

    // ── contains(): default method, delegates to get() ──────────────────
    println!("contains user:bob -> {}", store.contains(b"user:bob")?);
    println!("contains session:1 -> {}", store.contains(b"session:1")?);

    // ── get_many(): default method, one get() per key ────────────────────
    let many = store.get_many(&[b"user:alice", b"user:bob", b"user:zzz"])?;
    println!("get_many -> {many:?}");

    // ── get_ref(): default method, wraps get() in Cow::Owned ─────────────
    match store.get_ref(b"user:alice")? {
        Some(Cow::Owned(v)) => println!("get_ref (owned, MemKv has no zero-copy path) -> {v:?}"),
        Some(Cow::Borrowed(v)) => println!("get_ref (borrowed) -> {v:?}"),
        None => println!("get_ref -> None"),
    }

    // ── prefix_scan(): default method, computed via prefix_upper_bound() +
    //    range() ────────────────────────────────────────────────────────
    println!("prefix_scan(\"user:\"):");
    for item in store.prefix_scan(b"user:")? {
        let (k, v) = item?;
        println!(
            "  {} = {}",
            String::from_utf8_lossy(&k),
            String::from_utf8_lossy(&v)
        );
    }

    // `prefix_upper_bound` is the free function `prefix_scan` uses under the
    // hood — exposed directly for callers building their own range queries.
    println!(
        "prefix_upper_bound(b\"user:\") -> {:?}",
        prefix_upper_bound(b"user:").map(|b| String::from_utf8_lossy(&b).into_owned())
    );

    // ── range_rev(): default method, collects range() then reverses ─────
    store.put(b"n:1", b"a")?;
    store.put(b"n:2", b"b")?;
    store.put(b"n:3", b"c")?;
    let descending: Vec<Vec<u8>> = store
        .range_rev(b"n:", b"n;")?
        .map(|r| r.map(|(k, _v)| k))
        .collect::<Result<_, StoreError>>()?;
    println!(
        "range_rev(n:, n;) -> {:?}",
        descending
            .iter()
            .map(|k| String::from_utf8_lossy(k))
            .collect::<Vec<_>>()
    );

    // ── count() / keys(): default methods built on iter() ───────────────
    println!("count() -> {}", store.count()?);
    let keys: Vec<Vec<u8>> = store.keys()?.collect::<Result<_, StoreError>>()?;
    println!("keys() -> {} total", keys.len());

    // ── TTL trait methods: MemKv doesn't override them, so every one
    //    returns the trait's default `Unsupported` error. ─────────────────
    match store.put_with_ttl(b"expiring", b"soon", Duration::from_secs(1)) {
        Err(StoreError::Unsupported(msg)) => {
            println!("put_with_ttl (default) -> Unsupported: {msg}")
        }
        other => println!("put_with_ttl (default) -> unexpected: {other:?}"),
    }

    // The free `expiry_epoch_millis` / `is_expired` helpers that backends
    // *with* TTL support build on (see oxistore-kv-redb/-sled/-fjall).
    let expiry = expiry_epoch_millis(Duration::from_millis(50))?;
    println!(
        "expiry_epoch_millis(+50ms) -> {expiry}, is_expired now? {}",
        is_expired(expiry)
    );
    std::thread::sleep(Duration::from_millis(80));
    println!(
        "... after sleeping 80ms, is_expired? {}",
        is_expired(expiry)
    );

    // ── StoreConfig / StoreMetrics: backend-agnostic config & stats types ─
    let cfg = StoreConfig::default();
    println!(
        "StoreConfig::default() -> sync_writes={}, read_only={}, cache_size_bytes={:?}",
        cfg.sync_writes, cfg.read_only, cfg.cache_size_bytes
    );

    let metrics = StoreMetrics {
        reads: 80,
        cache_hits: 64,
        cache_misses: 16,
        ..StoreMetrics::default()
    };
    println!(
        "StoreMetrics: reads={}, cache_hit_rate={:.2}",
        metrics.reads,
        metrics.cache_hit_rate()
    );

    Ok(())
}

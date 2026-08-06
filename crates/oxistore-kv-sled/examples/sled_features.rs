//! Tour of `oxistore-kv-sled`-specific features that go beyond the generic
//! [`KvStore`] trait surface already covered by `oxistore`'s own examples.
//!
//! ```sh
//! cargo run -p oxistore-kv-sled --example sled_features
//! ```
//!
//! Covers: named trees ([`SledStore::open_tree`]), merge operators
//! ([`SledStore::set_merge_operator`] / [`SledStore::merge`]), prefix
//! event subscriptions ([`SledStore::watch_prefix`]), and
//! [`SledStoreBuilder`] with [`SledMode`].

use std::sync::{Arc, Barrier};
use std::time::Duration;

use oxistore_core::KvStore;
use oxistore_kv_sled::{SledMode, SledStoreBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── SledStoreBuilder: throughput-oriented mode + explicit segment size ─
    let store = Arc::new(
        SledStoreBuilder::new()
            .temporary(true)
            .mode(SledMode::HighThroughput)
            .segment_size(512 * 1024) // must be a power of two
            .build(
                std::env::temp_dir().join(format!("oxistore_sled_example_{}", std::process::id())),
            )?,
    );

    // ── Named trees: independent keyspaces (column families) ───────────
    // The default tree (used by `KvStore::put`/`get`/...) and named trees
    // returned by `open_tree` are fully isolated from each other.
    store.put(b"k", b"in-default-tree")?;
    let counters_tree = store.open_tree(b"counters")?;
    counters_tree.insert(b"k", b"in-counters-tree")?;

    println!(
        "default tree get(k)  -> {:?}",
        store
            .get(b"k")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );
    println!(
        "counters tree get(k) -> {:?}",
        counters_tree
            .get(b"k")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    // ── Merge operators: server-side read-modify-write ──────────────────
    // Registers a byte-string append merge function, then uses `merge()`
    // to accumulate a log without a get()+put() round trip per entry.
    store.set_merge_operator(|_key, old, new_piece| {
        let mut v = old.map(<[u8]>::to_vec).unwrap_or_default();
        if !v.is_empty() {
            v.push(b',');
        }
        v.extend_from_slice(new_piece);
        Some(v)
    });
    store.merge(b"log:1", b"start")?;
    store.merge(b"log:1", b"processing")?;
    store.merge(b"log:1", b"done")?;
    println!(
        "merged log:1 -> {:?}",
        store
            .get(b"log:1")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    // ── watch_prefix: subscribe to key-change events under a prefix ─────
    // `Subscriber` blocks (optionally with a timeout) until a matching
    // event arrives, so this uses a background writer thread + a barrier
    // to make the example deterministic without a fixed sleep.
    let mut subscriber = store.watch_prefix(b"evt:");
    let barrier = Arc::new(Barrier::new(2));
    let barrier_writer = Arc::clone(&barrier);
    let store_writer = Arc::clone(&store);
    let writer = std::thread::spawn(move || -> Result<(), oxistore_core::StoreError> {
        barrier_writer.wait();
        store_writer.put(b"evt:1", b"something happened")
    });
    barrier.wait();
    match subscriber.next_timeout(Duration::from_secs(5)) {
        Ok(event) => println!("watch_prefix(evt:) saw an event: {event:?}"),
        Err(_timeout) => println!("watch_prefix(evt:) timed out waiting for an event"),
    }
    match writer.join() {
        Ok(put_result) => put_result?,
        Err(_panic_payload) => return Err("writer thread panicked".into()),
    }

    // ── flush_with_reclaim: durable flush + on-disk size observability ──
    for i in 0..100u32 {
        store.put(format!("bulk:{i}").as_bytes(), &[0u8; 256])?;
    }
    let size_after_flush = store.flush_with_reclaim()?;
    println!("flush_with_reclaim() -> size_on_disk={size_after_flush} bytes");

    Ok(())
}

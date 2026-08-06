//! Tour of `oxistore-kv-fjall`-specific features that go beyond the generic
//! [`KvStore`] trait surface already covered by `oxistore`'s own examples.
//!
//! ```sh
//! cargo run -p oxistore-kv-fjall --example fjall_features
//! ```
//!
//! Covers: multi-keyspace partitions ([`FjallStore::open_partition`]),
//! atomic cross-partition writes ([`FjallStore::batch_write_across`]),
//! read-and-write helpers ([`FjallStore::put_returning`] /
//! [`FjallStore::delete_returning`]), a cross-keyspace consistent snapshot
//! ([`FjallStore::raw_snapshot`]), software write-rate-limiting
//! ([`FjallStore::rate_limiter`]), and [`FjallStoreBuilder`] with LZ4
//! compression.

use fjall::{CompressionType, Readable};
use oxistore_core::KvStore;
use oxistore_kv_fjall::FjallStoreBuilder;

/// Mirrors the private `PartitionWrites` alias `batch_write_across` accepts:
/// `(partition_name, pairs)` tuples where `pairs` is a slice of key/value
/// byte-slice pairs to insert into that partition.
type PartitionWrites<'a> = [(&'a str, Vec<(&'a [u8], &'a [u8])>)];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── FjallStoreBuilder: pure-Rust LZ4 block compression (see the crate's
    //    module docs for why this stays Pure Rust — fjall bundles lz4_flex,
    //    a native-Rust LZ4 port, not a C library) ─────────────────────────
    let dir = std::env::temp_dir().join(format!("oxistore_fjall_example_{}", std::process::id()));
    let store = FjallStoreBuilder::new()
        .compression_type(CompressionType::Lz4)
        .build(&dir)?;

    // ── Multi-keyspace partitions: independent LSM-tree column families ──
    let orders = store.open_partition("orders")?;
    let inventory = store.open_partition("inventory")?;
    orders.insert(b"order:1", b"widget x3")?;
    inventory.insert(b"widget", b"qty:97")?;
    // Partitions are fully isolated: the default keyspace (used by
    // `KvStore::put`/`get`) never sees these keys.
    println!(
        "default keyspace get(order:1) -> {:?}",
        store.get(b"order:1")?
    );
    println!(
        "orders partition get(order:1)   -> {:?}",
        orders
            .get(b"order:1")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    // ── batch_write_across: atomic write spanning multiple partitions ────
    // Placing an order and decrementing inventory must succeed or fail
    // together; a partial write would leave the two partitions inconsistent.
    let writes: &PartitionWrites<'_> = &[
        (
            "orders",
            vec![(b"order:2".as_slice(), b"widget x1".as_slice())],
        ),
        (
            "inventory",
            vec![(b"widget".as_slice(), b"qty:96".as_slice())],
        ),
    ];
    store.batch_write_across(writes)?;
    println!(
        "after batch_write_across: inventory widget -> {:?}",
        inventory
            .get(b"widget")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    // ── Cross-keyspace consistent snapshot ────────────────────────────────
    // `raw_snapshot()` returns a `fjall::Snapshot` that spans the *whole*
    // database, so reads against multiple partitions through the same
    // snapshot see a mutually consistent point-in-time view.
    let snap = store.raw_snapshot();
    let snap_order = snap.get(&orders, b"order:2")?;
    let snap_qty = snap.get(&inventory, b"widget")?;
    println!(
        "cross-keyspace snapshot: order:2={:?}, widget qty={:?}",
        snap_order.map(|v| String::from_utf8_lossy(&v).into_owned()),
        snap_qty.map(|v| String::from_utf8_lossy(&v).into_owned())
    );
    // Mutating after the snapshot was taken does not change the view above.
    inventory.insert(b"widget", b"qty:0")?;
    println!(
        "live inventory widget (post-mutation) -> {:?}, snapshot still sees -> {:?}",
        inventory
            .get(b"widget")?
            .map(|v| String::from_utf8_lossy(&v).into_owned()),
        snap.get(&inventory, b"widget")?
            .map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    // ── put_returning / delete_returning: read-and-write in one call ─────
    let previous = store.put_returning(b"counter", b"1")?;
    println!("put_returning(counter, 1) -> previous={previous:?}");
    let previous = store.put_returning(b"counter", b"2")?;
    println!("put_returning(counter, 2) -> previous={previous:?}");
    let removed = store.delete_returning(b"counter")?;
    println!("delete_returning(counter) -> removed={removed:?}");

    // ── rate_limiter: software token-bucket write throttling ─────────────
    // fjall 3.x has no built-in write-rate-limit API; this wraps `put`/
    // `delete` with a small sleep every N writes.
    let mut limited = store.rate_limiter(1_000, std::time::Duration::from_secs(1));
    for i in 0..5u32 {
        limited.put(format!("rl:{i}").as_bytes(), b"x")?;
    }
    println!("wrote 5 keys through the rate-limited writer");

    // Clean up the example's own temp directory.
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

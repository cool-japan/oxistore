//! Runnable tour of the core `KvStore` API.
//!
//! ```sh
//! cargo run -p oxistore --example kv_basics
//! ```
//!
//! Covers: `put` / `get` / `contains` / `delete`, ordered `range`,
//! `prefix_scan`, `iter`, and `count` — all through the backend-agnostic
//! [`KvStore`](oxistore::KvStore) trait over an in-memory redb store.

use oxistore::prelude::StoreKind;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open an ephemeral store (default backend: redb).
    let store = oxistore::open_in_memory(StoreKind::Redb)?;

    // ── put / get ────────────────────────────────────────────────────────────
    store.put(b"user:alice", b"Alice")?;
    store.put(b"user:bob", b"Bob")?;
    store.put(b"user:carol", b"Carol")?;
    store.put(b"session:1", b"active")?;

    println!("get user:alice -> {:?}", string(store.get(b"user:alice")?));
    println!("contains user:bob -> {}", store.contains(b"user:bob")?);
    println!("get missing -> {:?}", store.get(b"user:zzz")?);

    // ── delete ───────────────────────────────────────────────────────────────
    store.delete(b"session:1")?;
    println!(
        "after delete, contains session:1 -> {}",
        store.contains(b"session:1")?
    );

    // ── ordered range scan [lo, hi) ──────────────────────────────────────────
    println!("range [user:, user;):");
    for item in store.range(b"user:", b"user;")? {
        let (k, v) = item?;
        println!(
            "  {} = {}",
            String::from_utf8_lossy(&k),
            String::from_utf8_lossy(&v)
        );
    }

    // ── prefix scan (convenience over range) ─────────────────────────────────
    let user_count = store.prefix_scan(b"user:")?.count();
    println!("users with prefix 'user:' -> {user_count}");

    // ── full iteration + count ───────────────────────────────────────────────
    println!("all keys:");
    for item in store.iter()? {
        let (k, _v) = item?;
        println!("  {}", String::from_utf8_lossy(&k));
    }
    println!("total entries -> {}", store.count()?);

    Ok(())
}

fn string(v: Option<Vec<u8>>) -> Option<String> {
    v.map(|b| String::from_utf8_lossy(&b).into_owned())
}

//! Runnable demonstration of TTL (time-to-live) key expiry.
//!
//! ```sh
//! cargo run -p oxistore --example ttl_expiry
//! ```
//!
//! Shows `put_with_ttl`, remaining-`ttl` inspection, `expire`, `persist`, and
//! `purge_expired`.  Expired keys are transparently hidden from `get`, scans,
//! and snapshots.

use std::time::Duration;

use oxistore::prelude::StoreKind;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = oxistore::open_in_memory(StoreKind::Redb)?;

    // ── put_with_ttl: key auto-expires ───────────────────────────────────────
    store.put_with_ttl(b"otp:1234", b"secret", Duration::from_millis(100))?;
    println!("otp present now -> {}", store.contains(b"otp:1234")?);
    if let Some(remaining) = store.ttl(b"otp:1234")? {
        println!("otp remaining ttl -> {remaining:?}");
    }

    std::thread::sleep(Duration::from_millis(200));
    println!(
        "otp present after 200ms -> {}",
        store.contains(b"otp:1234")?
    );

    // ── expire: attach a TTL to an existing key ──────────────────────────────
    store.put(b"cache:page", b"<html>")?;
    store.expire(b"cache:page", Duration::from_millis(100))?;

    // ── persist: remove a TTL so the key survives ────────────────────────────
    store.put_with_ttl(b"keep:me", b"v", Duration::from_millis(100))?;
    let had_ttl = store.persist(b"keep:me")?;
    println!("persist removed a ttl -> {had_ttl}");

    std::thread::sleep(Duration::from_millis(200));
    println!("cache:page after expiry -> {:?}", store.get(b"cache:page")?);
    println!("keep:me survives -> {:?}", string(store.get(b"keep:me")?));

    // ── purge_expired: physically reclaim expired entries ────────────────────
    store.put_with_ttl(b"tmp:a", b"1", Duration::from_millis(50))?;
    store.put_with_ttl(b"tmp:b", b"2", Duration::from_millis(50))?;
    std::thread::sleep(Duration::from_millis(120));
    let purged = store.purge_expired()?;
    println!("purge_expired reclaimed {purged} entries");

    Ok(())
}

fn string(v: Option<Vec<u8>>) -> Option<String> {
    v.map(|b| String::from_utf8_lossy(&b).into_owned())
}

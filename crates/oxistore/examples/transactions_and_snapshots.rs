//! Runnable demonstration of transactions and snapshots.
//!
//! ```sh
//! cargo run -p oxistore --example transactions_and_snapshots
//! ```
//!
//! Shows atomic multi-key writes with read-your-writes semantics, rollback,
//! and point-in-time snapshot isolation.

use oxistore::prelude::StoreKind;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = oxistore::open_in_memory(StoreKind::Redb)?;
    store.put(b"balance:acct", b"100")?;

    // ── Transaction: read-your-writes then commit atomically ─────────────────
    {
        let mut txn = store.transaction()?;
        txn.put(b"balance:acct", b"140")?;
        txn.put(b"audit:last", b"deposit+40")?;

        // Uncommitted writes are visible *within* the transaction.
        println!(
            "inside txn, balance -> {:?}",
            string(txn.get(b"balance:acct")?)
        );
        txn.commit()?;
    }
    println!(
        "after commit, balance -> {:?}",
        string(store.get(b"balance:acct")?)
    );

    // ── Transaction: rollback discards staged writes ─────────────────────────
    {
        let mut txn = store.transaction()?;
        txn.put(b"balance:acct", b"999")?;
        txn.rollback()?;
    }
    println!(
        "after rollback, balance -> {:?} (unchanged)",
        string(store.get(b"balance:acct")?)
    );

    // ── Snapshot: an immutable point-in-time view ────────────────────────────
    let snap = store.snapshot()?;
    store.put(b"balance:acct", b"200")?; // mutate the live store afterwards

    println!(
        "snapshot still sees balance -> {:?}",
        string(snap.get(b"balance:acct")?)
    );
    println!(
        "live store now sees balance -> {:?}",
        string(store.get(b"balance:acct")?)
    );

    Ok(())
}

fn string(v: Option<Vec<u8>>) -> Option<String> {
    v.map(|b| String::from_utf8_lossy(&b).into_owned())
}

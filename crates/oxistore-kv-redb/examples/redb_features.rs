//! Tour of `oxistore-kv-redb`-specific features that go beyond the generic
//! [`KvStore`] trait surface already covered by `oxistore`'s own examples.
//!
//! ```sh
//! cargo run -p oxistore-kv-redb --example redb_features
//! ```
//!
//! Covers: [`RedbStoreBuilder`] (custom table name), [`RedbStore::put_returning_old`] /
//! [`RedbStore::delete_returning_old`], lazy [`RedbStore::scan_iter`], and
//! [`TypedRedbTable`] for serde-JSON-typed values.

use oxistore_core::KvStore;
use oxistore_kv_redb::{RedbStore, RedbStoreBuilder, TypedRedbTable};
use serde::{Deserialize, Serialize};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── RedbStoreBuilder: custom table name + cache size ────────────────
    let store = RedbStoreBuilder::new()
        .table_name("inventory")
        .cache_size(4 * 1024 * 1024) // 4 MiB block cache
        .build_in_memory()?;

    // ── put_returning_old / delete_returning_old: atomic read-and-write ──
    // Useful for "what was the previous value" without a separate get() +
    // put() round trip (and the associated TOCTOU race under concurrency).
    let displaced = store.put_returning_old(b"widget:1", b"qty:10")?;
    println!("first put_returning_old -> {displaced:?} (no prior value)");

    let displaced = store.put_returning_old(b"widget:1", b"qty:7")?;
    println!(
        "second put_returning_old -> {:?} (was qty:10, sold 3)",
        displaced.map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    let removed = store.delete_returning_old(b"widget:1")?;
    println!(
        "delete_returning_old -> {:?}",
        removed.map(|v| String::from_utf8_lossy(&v).into_owned())
    );

    // ── scan_iter: lazy range iteration over a live ReadTransaction ─────
    // `KvStore::range` must materialise into a `Vec` (the trait's return
    // type can't borrow from a local transaction), but `scan_iter` holds
    // the `ReadTransaction` open and decodes rows one at a time as the
    // caller advances the iterator — useful for wide scans where
    // allocating the whole result up front is wasteful.
    for i in 0..5u32 {
        store.put(
            format!("item:{i:03}").as_bytes(),
            format!("value-{i}").as_bytes(),
        )?;
    }
    println!("scan_iter(item:000, item:999):");
    let lazy_iter = store.scan_iter(b"item:000", b"item:999")?;
    for entry in lazy_iter {
        let (k, v) = entry?;
        println!(
            "  {} = {}",
            String::from_utf8_lossy(&k),
            String::from_utf8_lossy(&v)
        );
    }

    // ── TypedRedbTable: serde-JSON typed values over a plain RedbStore ───
    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Account {
        owner: String,
        balance_cents: i64,
    }

    let typed_store = RedbStore::open_in_memory()?;
    let accounts = TypedRedbTable::new(typed_store);

    accounts.typed_put(
        "acct:1",
        &Account {
            owner: "Alice".to_string(),
            balance_cents: 12_345,
        },
    )?;

    let loaded: Option<Account> = accounts.typed_get("acct:1")?;
    println!("typed_get(acct:1) -> {loaded:?}");
    assert_eq!(
        loaded,
        Some(Account {
            owner: "Alice".to_string(),
            balance_cents: 12_345,
        })
    );

    accounts.typed_delete("acct:1")?;
    println!(
        "after typed_delete, typed_get -> {:?}",
        accounts.typed_get::<Account>("acct:1")?
    );

    // ── open_with_recovery: transparent crash-recovery open ─────────────
    // Opens a file-backed store; if the file is corrupt, it is recreated
    // automatically and the returned bool tells the caller a repair
    // happened (so it can log the event / restore from backup).
    let path =
        std::env::temp_dir().join(format!("oxistore_redb_example_{}.redb", std::process::id()));
    let (recovered_store, was_repaired) = RedbStore::open_with_recovery(&path)?;
    recovered_store.put(b"k", b"v")?;
    println!("open_with_recovery(fresh file) -> was_repaired={was_repaired}");
    // Clean up the example's own temp file.
    drop(recovered_store);
    let _ = std::fs::remove_file(&path);

    Ok(())
}

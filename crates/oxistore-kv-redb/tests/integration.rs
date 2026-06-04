//! Integration tests for `oxistore-kv-redb`.
//!
//! These tests verify that `RedbStore` integrates correctly with:
//! 1. The `oxistore` facade — `open_with(StoreKind::Redb, path)`.
//! 2. `CacheableKvStore` from `oxistore-cache`.
//!
//! Also tests concurrent reader support (multiple simultaneous `ReadTransaction`
//! instances) and crash/corruption recovery.

use oxistore_core::KvStore;
use oxistore_kv_redb::{RedbIter, RedbStore, TypedRedbTable};

// ── helpers ──────────────────────────────────────────────────────────────────

fn temp_db_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxistore_redb_integ_{}_{}_{}.redb",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

// ── Integration: oxistore facade ─────────────────────────────────────────────

#[test]
fn facade_open_with_redb_roundtrip() {
    use oxistore::{open_with, StoreKind};

    let path = temp_db_path("facade_roundtrip");
    let store = open_with(StoreKind::Redb, &path).expect("facade open_with failed");

    store.put(b"facade_key", b"facade_value").expect("put");
    let got = store.get(b"facade_key").expect("get");
    assert_eq!(
        got.as_deref(),
        Some(b"facade_value".as_ref()),
        "facade open_with roundtrip failed"
    );

    let count = store.count().expect("count");
    assert_eq!(count, 1);

    store.delete(b"facade_key").expect("delete");
    assert_eq!(store.get(b"facade_key").expect("get after delete"), None);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn facade_open_in_memory_redb() {
    use oxistore::{open_in_memory, StoreKind};

    let store = open_in_memory(StoreKind::Redb).expect("facade open_in_memory failed");
    store.put(b"mem_key", b"mem_val").expect("put");
    assert_eq!(
        store.get(b"mem_key").expect("get").as_deref(),
        Some(b"mem_val".as_ref())
    );
}

#[test]
fn facade_range_scan_redb() {
    use oxistore::{open_in_memory, StoreKind};

    let store = open_in_memory(StoreKind::Redb).expect("facade open");
    for i in 0..20u32 {
        let k = format!("key_{i:04}").into_bytes();
        let v = i.to_le_bytes().to_vec();
        store.put(&k, &v).expect("put");
    }

    let lo = b"key_0005".as_ref();
    let hi = b"key_0015".as_ref();
    let results: Vec<_> = store
        .range(lo, hi)
        .expect("range")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(results.len(), 10, "range should return 10 keys");
}

// ── Integration: CacheableKvStore ────────────────────────────────────────────

#[test]
fn cacheable_kv_store_wraps_redb() {
    use oxistore_cache::{CacheableKvStore, LruCache};

    let store = RedbStore::open_in_memory().expect("open");
    let cache: LruCache<Vec<u8>, Vec<u8>> = LruCache::new(16);
    let cacheable = CacheableKvStore::new(store, cache);

    // Put through the cache adapter
    cacheable.put(b"cached_key", b"cached_value").expect("put");

    // First get — cache miss, loads from store
    let got = cacheable.get(b"cached_key").expect("get");
    assert_eq!(
        got.as_deref(),
        Some(b"cached_value".as_ref()),
        "cache miss get"
    );

    // Second get — cache hit (value is now in LRU cache)
    let got2 = cacheable.get(b"cached_key").expect("second get");
    assert_eq!(
        got2.as_deref(),
        Some(b"cached_value".as_ref()),
        "cache hit get"
    );

    // Overwrite invalidates cache
    cacheable
        .put(b"cached_key", b"new_value")
        .expect("overwrite");
    let got3 = cacheable.get(b"cached_key").expect("get after overwrite");
    assert_eq!(
        got3.as_deref(),
        Some(b"new_value".as_ref()),
        "post-overwrite get"
    );

    // Delete invalidates cache
    cacheable.delete(b"cached_key").expect("delete");
    let got4 = cacheable.get(b"cached_key").expect("get after delete");
    assert_eq!(got4, None);
}

#[test]
fn cacheable_kv_store_missing_key_returns_none() {
    use oxistore_cache::{CacheableKvStore, LruCache};

    let store = RedbStore::open_in_memory().expect("open");
    let cache: LruCache<Vec<u8>, Vec<u8>> = LruCache::new(8);
    let cacheable = CacheableKvStore::new(store, cache);

    // Key never written — should return None
    let got = cacheable.get(b"nonexistent").expect("get nonexistent");
    assert_eq!(got, None);
}

#[test]
fn cacheable_kv_store_count_and_iter_delegate_to_store() {
    use oxistore_cache::{CacheableKvStore, LruCache};

    let store = RedbStore::open_in_memory().expect("open");
    let cache: LruCache<Vec<u8>, Vec<u8>> = LruCache::new(32);
    let cacheable = CacheableKvStore::new(store, cache);

    for i in 0..10u32 {
        cacheable
            .put(format!("k{i}").as_bytes(), b"v")
            .expect("put");
    }

    let count = cacheable.count().expect("count");
    assert_eq!(count, 10, "count must reflect all 10 keys");

    let items: Vec<_> = cacheable
        .iter()
        .expect("iter")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(items.len(), 10);
}

// ── Concurrent reader support ─────────────────────────────────────────────────

/// Verify that multiple simultaneous snapshots (each backed by a distinct
/// `redb::ReadTransaction`) can be held and read concurrently without errors.
/// This documents and tests redb's multi-reader MVCC guarantee.
///
/// Each snapshot is used from the thread that created it — snapshots are
/// not moved across threads because `Box<dyn KvSnapshot>` is not `Send`.
/// The key point tested is that *holding* multiple read transactions at the
/// same time does not block each other or concurrent writes.
#[test]
fn multiple_concurrent_read_transactions() {
    use std::sync::Arc;

    let store = Arc::new(RedbStore::open_in_memory().expect("open"));

    // Pre-populate the store
    for i in 0..50u32 {
        let k = format!("mk_{i:04}").into_bytes();
        let v = i.to_le_bytes().to_vec();
        store.put(&k, &v).expect("put");
    }

    // Open 8 snapshots simultaneously — each holds a distinct ReadTransaction.
    // This verifies that redb allows multiple concurrent readers without error.
    let snapshots: Vec<_> = (0..8)
        .map(|_| store.snapshot().expect("snapshot"))
        .collect();

    // All snapshots are held at the same time — reads must succeed on every one.
    for (idx, snap) in snapshots.iter().enumerate() {
        let key = format!("mk_{:04}", idx * 6);
        let got = snap.get(key.as_bytes()).expect("snap get");
        assert!(got.is_some(), "snapshot {idx} should see key {key}");

        // Range scan from each snapshot
        let results: Vec<_> = snap
            .range(b"mk_0000", b"mk_0020")
            .expect("snap range")
            .collect::<Result<_, _>>()
            .expect("snap range collect");
        assert_eq!(
            results.len(),
            20,
            "snapshot {idx} range scan must return 20 items"
        );
    }

    // While all 8 snapshots are still open, perform a write — must not block or error.
    store
        .put(b"post_snapshot_key", b"val")
        .expect("write while snapshots held");

    // Snapshots still see pre-write state
    for snap in &snapshots {
        let new_val = snap.get(b"post_snapshot_key").expect("snap get new key");
        assert_eq!(new_val, None, "snapshots must not see post-snapshot write");
    }
}

/// Verify concurrent reads from `RedbStore::snapshot` don't block writes.
///
/// This test uses `RedbStore` directly through an `Arc` so that a writer
/// thread can hold a reference to the live store while the main thread holds
/// a snapshot.
#[test]
fn concurrent_reads_do_not_block_writes() {
    use std::sync::Arc;
    use std::thread;

    let store = Arc::new(RedbStore::open_in_memory().expect("open"));
    store.put(b"base", b"base_val").expect("initial put");

    // Hold a snapshot open.  The snapshot captures state before the writes below.
    let snap = store.snapshot().expect("snapshot");

    // Write through the live store from a separate thread while holding the snapshot.
    let store_clone = Arc::clone(&store);
    let writer = thread::spawn(move || {
        for i in 0..20u32 {
            let k = format!("write_{i:04}").into_bytes();
            store_clone.put(&k, b"v").expect("concurrent write");
        }
    });

    writer.join().expect("writer panicked");

    // Snapshot still sees only the state at creation time.
    let from_snap = snap.get(b"base").expect("snap get");
    assert_eq!(from_snap.as_deref(), Some(b"base_val".as_ref()));

    // Snapshot does NOT see the newly written keys (MVCC isolation).
    let new_key = snap.get(b"write_0000").expect("snap get new");
    assert_eq!(new_key, None, "snapshot must not see post-snapshot writes");

    // Live store sees all 20 new keys plus the original.
    let count = store.count().expect("count");
    assert_eq!(
        count, 21,
        "live store should have 21 keys after concurrent writes"
    );
}

// ── Crash / corruption recovery ───────────────────────────────────────────────

/// Write data, then corrupt the file by truncating it, then verify that
/// `open_with_recovery` handles the corruption gracefully.
#[test]
fn crash_recovery_handles_corrupted_file() {
    let path = temp_db_path("crash_recovery");

    // Phase 1: create a valid database with some data
    {
        let store = RedbStore::open(&path).expect("initial open");
        for i in 0..20u32 {
            let k = format!("cr_{i:04}").into_bytes();
            store.put(&k, b"data").expect("put");
        }
        // Drop to close the DB and flush all writes
    }

    assert!(path.exists(), "database file should exist after writing");

    // Phase 2: corrupt the file by overwriting the first 64 bytes with garbage
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open for corruption");
        f.seek(SeekFrom::Start(0)).expect("seek");
        f.write_all(&[0xDE, 0xAD, 0xBE, 0xEF].repeat(16))
            .expect("corrupt write");
        f.flush().expect("flush corrupt");
    }

    // Phase 3: attempt normal open — should fail with corruption error
    let normal_open_result = RedbStore::open(&path);
    assert!(
        normal_open_result.is_err(),
        "opening a corrupted database should fail"
    );

    // Phase 4: open with recovery — should succeed (by recreating the db)
    let (recovered_store, was_repaired) =
        RedbStore::open_with_recovery(&path).expect("open_with_recovery should succeed");

    // The store was recreated (data lost, but no panic or error)
    assert!(
        was_repaired,
        "recovery should report that repair was performed"
    );

    // The recovered store is functional
    recovered_store
        .put(b"after_recovery", b"works")
        .expect("post-recovery put");
    let val = recovered_store
        .get(b"after_recovery")
        .expect("post-recovery get");
    assert_eq!(val.as_deref(), Some(b"works".as_ref()));

    let _ = std::fs::remove_file(&path);
}

/// `open_with_recovery` on a healthy database should return `repaired=false`.
#[test]
fn recovery_on_clean_database_returns_false() {
    let path = temp_db_path("clean_recovery");

    {
        let store = RedbStore::open(&path).expect("create");
        store.put(b"existing", b"data").expect("put");
    }

    let (store, repaired) =
        RedbStore::open_with_recovery(&path).expect("open_with_recovery on clean db");
    assert!(!repaired, "clean database should not require repair");
    let got = store.get(b"existing").expect("get");
    assert_eq!(got.as_deref(), Some(b"data".as_ref()));

    let _ = std::fs::remove_file(&path);
}

// ── Return old value from put / delete ────────────────────────────────────────

#[test]
fn put_returning_old_gives_displaced_value() {
    let store = RedbStore::open_in_memory().expect("open");

    // First insert — no old value
    let old = store
        .put_returning_old(b"key", b"v1")
        .expect("put_returning_old");
    assert_eq!(old, None, "first insert should return None");

    // Overwrite — returns previous value
    let old2 = store.put_returning_old(b"key", b"v2").expect("overwrite");
    assert_eq!(
        old2.as_deref(),
        Some(b"v1".as_ref()),
        "should return old v1"
    );

    // Overwrite again
    let old3 = store.put_returning_old(b"key", b"v3").expect("overwrite 2");
    assert_eq!(old3.as_deref(), Some(b"v2".as_ref()));
}

#[test]
fn delete_returning_old_gives_removed_value() {
    let store = RedbStore::open_in_memory().expect("open");

    // Delete absent key — returns None
    let old = store.delete_returning_old(b"ghost").expect("delete absent");
    assert_eq!(old, None);

    // Insert then delete
    store.put(b"key", b"value").expect("put");
    let old2 = store.delete_returning_old(b"key").expect("delete");
    assert_eq!(old2.as_deref(), Some(b"value".as_ref()));

    // Key no longer exists
    assert_eq!(store.get(b"key").expect("get"), None);
}

// ── TypedRedbTable ────────────────────────────────────────────────────────────

#[test]
fn typed_table_put_get_roundtrip() {
    let store = RedbStore::open_in_memory().expect("open");
    let table = TypedRedbTable::new(store);

    table.typed_put("counter", &42u64).expect("typed_put");
    let v: Option<u64> = table.typed_get("counter").expect("typed_get");
    assert_eq!(v, Some(42u64));
}

#[test]
fn typed_table_get_missing_returns_none() {
    let store = RedbStore::open_in_memory().expect("open");
    let table = TypedRedbTable::new(store);

    let v: Option<u64> = table.typed_get("nonexistent").expect("typed_get missing");
    assert_eq!(v, None);
}

#[test]
fn typed_table_delete() {
    let store = RedbStore::open_in_memory().expect("open");
    let table = TypedRedbTable::new(store);

    table.typed_put("to_delete", &99u32).expect("put");
    table.typed_delete("to_delete").expect("delete");
    let v: Option<u32> = table.typed_get("to_delete").expect("get after delete");
    assert_eq!(v, None);
}

#[test]
fn typed_table_overwrite() {
    let store = RedbStore::open_in_memory().expect("open");
    let table = TypedRedbTable::new(store);

    table.typed_put("key", &"first_value").expect("first put");
    table.typed_put("key", &"second_value").expect("second put");
    let v: Option<String> = table.typed_get("key").expect("get");
    assert_eq!(v.as_deref(), Some("second_value"));
}

#[test]
fn typed_table_json_complex_value() {
    use std::collections::HashMap;

    let store = RedbStore::open_in_memory().expect("open");
    let table = TypedRedbTable::new(store);

    let mut map = HashMap::new();
    map.insert("alpha".to_string(), 1u32);
    map.insert("beta".to_string(), 2u32);
    map.insert("gamma".to_string(), 3u32);

    table.typed_put("map_key", &map).expect("put map");
    let got: Option<HashMap<String, u32>> = table.typed_get("map_key").expect("get map");
    let got_map = got.expect("should exist");
    assert_eq!(got_map["alpha"], 1);
    assert_eq!(got_map["beta"], 2);
    assert_eq!(got_map["gamma"], 3);
}

#[test]
fn typed_table_iter_raw_returns_all_entries() {
    let store = RedbStore::open_in_memory().expect("open");
    let table = TypedRedbTable::new(store);

    for i in 0..5u32 {
        let key = format!("item_{i:02}");
        table.typed_put(&key, &i).expect("typed_put");
    }

    let raw = table.iter_raw().expect("iter_raw");
    assert_eq!(raw.len(), 5, "iter_raw should return all 5 entries");
    // Keys should be UTF-8 "item_00" .. "item_04"
    for (k, _v) in &raw {
        assert!(k.starts_with("item_"), "key {k} should start with 'item_'");
    }
}

#[test]
fn typed_table_into_inner_recovers_store() {
    let store = RedbStore::open_in_memory().expect("open");
    let table = TypedRedbTable::new(store);

    table.typed_put("x", &123i32).expect("put");

    // Recover the inner store
    let inner = table.into_inner();
    let raw = inner.get(b"x").expect("raw get");
    assert!(
        raw.is_some(),
        "inner store should hold the serialized value"
    );
}

// ── RedbIter — streaming range iterator ──────────────────────────────────────

/// Verify that `range_iter` returns an `ExactSizeIterator` with the correct count.
#[test]
fn redb_iter_range_exact_size() {
    let store = RedbStore::open_in_memory().expect("open");
    for i in 0u32..20 {
        let k = format!("ri_{i:04}").into_bytes();
        store.put(&k, &i.to_le_bytes()).expect("put");
    }

    let iter: RedbIter = store
        .range_iter(b"ri_0005", b"ri_0015")
        .expect("range_iter");
    assert_eq!(iter.len(), 10, "range_iter should return exactly 10 items");
    assert!(!iter.is_empty());

    // Consume and count
    let items: Vec<_> = iter.collect::<Result<_, _>>().expect("collect");
    assert_eq!(items.len(), 10);
}

/// Verify `iter_collected` covers all keys.
#[test]
fn redb_iter_collected_full_scan() {
    let store = RedbStore::open_in_memory().expect("open");
    for i in 0u32..30 {
        let k = format!("full_{i:04}").into_bytes();
        store.put(&k, b"v").expect("put");
    }

    let iter: RedbIter = store.iter_collected().expect("iter_collected");
    assert_eq!(iter.len(), 30);
    let count = iter.count();
    assert_eq!(count, 30);
}

/// Verify `DoubleEndedIterator` — reverse iteration over a range.
#[test]
fn redb_iter_double_ended_reverse() {
    let store = RedbStore::open_in_memory().expect("open");
    for i in 0u32..10 {
        let k = format!("de_{i:04}").into_bytes();
        store.put(&k, &i.to_le_bytes()).expect("put");
    }

    let iter: RedbIter = store
        .range_iter(b"de_0000", b"de_0010")
        .expect("range_iter");
    let reversed: Vec<_> = iter
        .rev()
        .collect::<Result<_, _>>()
        .expect("collect reversed");

    assert_eq!(reversed.len(), 10);
    // First item in reversed order is the last key: de_0009
    assert_eq!(reversed[0].0, b"de_0009".to_vec());
    assert_eq!(reversed[9].0, b"de_0000".to_vec());
}

/// Verify `prefix_iter` returns only matching keys and has correct `len()`.
#[test]
fn redb_iter_prefix_scan() {
    let store = RedbStore::open_in_memory().expect("open");
    for i in 0u32..15 {
        store
            .put(format!("pfx_{i:04}").as_bytes(), b"v")
            .expect("put");
        store
            .put(format!("other_{i:04}").as_bytes(), b"v")
            .expect("put other");
    }

    let iter: RedbIter = store.prefix_iter(b"pfx_").expect("prefix_iter");
    assert_eq!(iter.len(), 15, "prefix_iter should find 15 matching keys");

    let items: Vec<_> = iter.collect::<Result<_, _>>().expect("collect");
    for (k, _) in &items {
        assert!(k.starts_with(b"pfx_"), "all keys should start with 'pfx_'");
    }
}

/// Verify empty iterator reports `is_empty()` correctly.
#[test]
fn redb_iter_empty_range() {
    let store = RedbStore::open_in_memory().expect("open");
    store.put(b"z_key", b"v").expect("put");

    // Range that matches nothing
    let iter: RedbIter = store.range_iter(b"a_", b"b_").expect("range_iter empty");
    assert!(iter.is_empty(), "empty range should report is_empty");
    assert_eq!(iter.len(), 0);
}

use oxistore_core::KvStore;
use oxistore_kv_fjall::{CompactionStrategyKind, FjallStore, FjallStoreBuilder};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> u64 {
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    nanos ^ unique_suffix()
}

fn open_temp() -> FjallStore {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-new-{}-{}-{}",
        std::process::id(),
        rand_suffix(),
        unique_suffix()
    ));
    FjallStore::open(&dir).expect("open failed")
}

// -- prefix_scan --

#[test]
fn prefix_scan_basic() {
    let store = open_temp();
    store.put(b"user:1", b"alice").expect("put");
    store.put(b"user:2", b"bob").expect("put");
    store.put(b"order:1", b"item").expect("put");

    let results: Vec<_> = store
        .prefix_scan(b"user:")
        .expect("prefix_scan")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, b"user:1");
    assert_eq!(results[1].0, b"user:2");
}

// -- batch --

#[test]
fn batch_write_and_delete() {
    let store = open_temp();
    let pairs: Vec<(&[u8], &[u8])> = vec![(b"a", b"1"), (b"b", b"2"), (b"c", b"3")];
    store.batch_write(&pairs).expect("batch_write");
    assert_eq!(store.count().expect("count"), 3);

    store.batch_delete(&[b"a", b"c"]).expect("batch_delete");
    assert_eq!(store.count().expect("count"), 1);
}

// -- count / iter / keys --

#[test]
fn count_correct() {
    let store = open_temp();
    assert_eq!(store.count().expect("count"), 0);
    store.put(b"a", b"1").expect("put");
    store.put(b"b", b"2").expect("put");
    assert_eq!(store.count().expect("count"), 2);
}

#[test]
fn iter_returns_all() {
    let store = open_temp();
    store.put(b"c", b"3").expect("put");
    store.put(b"a", b"1").expect("put");
    let items: Vec<_> = store
        .iter()
        .expect("iter")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].0, b"a");
}

#[test]
fn keys_only() {
    let store = open_temp();
    store.put(b"x", b"big").expect("put");
    let keys: Vec<_> = store
        .keys()
        .expect("keys")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], b"x");
}

// -- txn read-your-writes --

#[test]
fn txn_read_your_writes() {
    let store = open_temp();
    store.put(b"pre", b"v").expect("put");

    let mut txn = store.transaction().expect("txn");
    txn.put(b"new", b"buffered").expect("put");
    assert_eq!(txn.get(b"new").expect("get"), Some(b"buffered".to_vec()));
    assert_eq!(txn.get(b"pre").expect("get"), Some(b"v".to_vec()));

    txn.delete(b"pre").expect("delete");
    assert_eq!(txn.get(b"pre").expect("get"), None);

    txn.commit().expect("commit");
    assert_eq!(store.get(b"new").expect("get"), Some(b"buffered".to_vec()));
    assert_eq!(store.get(b"pre").expect("get"), None);
}

#[test]
fn txn_range_with_overlay() {
    let store = open_temp();
    store.put(b"b", b"committed").expect("put");

    let mut txn = store.transaction().expect("txn");
    txn.put(b"a", b"buf_a").expect("put");
    txn.put(b"c", b"buf_c").expect("put");

    let range: Vec<_> = txn
        .range(b"a", b"d")
        .expect("range")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(range.len(), 3);
    txn.commit().expect("commit");
}

// -- size_on_disk --

#[test]
fn size_on_disk_positive() {
    let store = open_temp();
    store.put(b"x", b"data").expect("put");
    let size = store.size_on_disk().expect("size");
    assert!(size > 0);
}

// -- fjall-column-family --

#[test]
fn column_families_are_isolated() {
    let store = open_temp();
    let family_a = store.open_partition("family_a").expect("open family_a");
    let family_b = store.open_partition("family_b").expect("open family_b");

    family_a
        .insert(b"shared_key", b"from-a")
        .expect("insert into family_a");
    family_b
        .insert(b"shared_key", b"from-b")
        .expect("insert into family_b");

    let a_val = family_a.get(b"shared_key").expect("get from family_a");
    let b_val = family_b.get(b"shared_key").expect("get from family_b");

    assert_eq!(a_val.as_deref(), Some(b"from-a".as_ref()));
    assert_eq!(b_val.as_deref(), Some(b"from-b".as_ref()));
}

#[test]
fn column_family_isolated_from_default() {
    let store = open_temp();
    store.put(b"key", b"in-default").expect("put in default");
    let named = store.open_partition("other_cf").expect("open partition");

    let val = named.get(b"key").expect("get from named partition");
    assert!(
        val.is_none(),
        "named partition must not see default keyspace keys"
    );
}

// -- fjall-backup and fjall-restore --

#[test]
fn backup_and_restore_roundtrip() {
    let store = open_temp();
    store.put(b"alpha", b"1").expect("put alpha");
    store.put(b"beta", b"2").expect("put beta");
    store.put(b"gamma", b"3").expect("put gamma");

    let backup_path = std::env::temp_dir().join(format!(
        "oxistore-fjall-backup-{}-{}.bin",
        std::process::id(),
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        }
    ));
    let restore_dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-restore-{}-{}",
        std::process::id(),
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        }
    ));

    store.backup(&backup_path).expect("backup");

    let restored = FjallStore::restore_from_backup(&backup_path, &restore_dir).expect("restore");

    assert_eq!(
        restored.get(b"alpha").expect("get alpha"),
        Some(b"1".to_vec())
    );
    assert_eq!(
        restored.get(b"beta").expect("get beta"),
        Some(b"2".to_vec())
    );
    assert_eq!(
        restored.get(b"gamma").expect("get gamma"),
        Some(b"3".to_vec())
    );

    // Cleanup
    let _ = std::fs::remove_file(&backup_path);
}

// ------------------------------------------------------------------
// FjallStoreBuilder tests
// ------------------------------------------------------------------

#[test]
fn fjall_builder_build_and_crud() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-builder-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let store = FjallStoreBuilder::new().build(&dir).expect("build failed");

    store.put(b"key1", b"val1").expect("put");
    assert_eq!(store.get(b"key1").expect("get"), Some(b"val1".to_vec()));
    store.delete(b"key1").expect("delete");
    assert_eq!(store.get(b"key1").expect("get"), None);
}

#[test]
fn fjall_clone() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-clone-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let store = FjallStore::open(&dir).expect("open");
    let cloned = store.clone();

    // Write via the clone; read back through the original.
    cloned.put(b"shared", b"data").expect("put via clone");
    assert_eq!(
        store.get(b"shared").expect("get from original"),
        Some(b"data".to_vec())
    );
}

#[test]
fn fjall_list_keyspaces() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-ks-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let store = FjallStore::open(&dir).expect("open");

    // At minimum "default" and "__ttl__" keyspaces must be present.
    let names = store.list_keyspaces().expect("list_keyspaces");
    assert!(
        !names.is_empty(),
        "list_keyspaces must return at least one entry"
    );
    assert!(
        names.contains(&"default".to_string()),
        "list_keyspaces must include 'default': {names:?}"
    );
}

// ------------------------------------------------------------------
// Bloom filter + compaction config tests (Slice 3)
// ------------------------------------------------------------------

#[test]
fn test_fjall_bloom_builder_opens() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-bloom-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let store = FjallStoreBuilder::new()
        .bloom_filter_bits_per_key(10.0)
        .build(&dir)
        .expect("build with bloom config");
    store.put(b"k", b"v").expect("put");
    assert_eq!(store.get(b"k").expect("get"), Some(b"v".to_vec()));
}

#[test]
fn test_fjall_bloom_and_compaction_builder() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-bloom-cmp-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let store = FjallStoreBuilder::new()
        .bloom_filter_bits_per_key(8.0)
        .compaction_strategy_kind(CompactionStrategyKind::Leveled)
        .build(&dir)
        .expect("build with bloom + compaction config");
    store.put(b"hello", b"world").expect("put");
    assert_eq!(store.get(b"hello").expect("get"), Some(b"world".to_vec()));
    store.delete(b"hello").expect("delete");
    assert_eq!(store.get(b"hello").expect("get"), None);
}

#[test]
fn test_fjall_batch_write_across_two_partitions() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-batch-across-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let store = FjallStoreBuilder::new().build(&dir).expect("build");

    let pairs_a: Vec<(&[u8], &[u8])> = vec![(b"a_key", b"a_val")];
    let pairs_b: Vec<(&[u8], &[u8])> = vec![(b"b_key", b"b_val")];
    store
        .batch_write_across(&[("partition_a", pairs_a), ("partition_b", pairs_b)])
        .expect("batch_write_across");

    // Verify data landed in the named partitions.
    let part_a = store
        .open_partition("partition_a")
        .expect("open partition_a");
    let part_b = store
        .open_partition("partition_b")
        .expect("open partition_b");
    assert_eq!(
        part_a.get(b"a_key").expect("get a_key").as_deref(),
        Some(b"a_val".as_ref())
    );
    assert_eq!(
        part_b.get(b"b_key").expect("get b_key").as_deref(),
        Some(b"b_val".as_ref())
    );
    // Data from partition_a must not appear in partition_b (isolation check).
    assert!(
        part_b.get(b"a_key").expect("cross-check").is_none(),
        "partitions must be isolated"
    );
}

#[test]
fn test_fjall_batch_write_across_empty_is_noop() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-batch-noop-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let store = FjallStoreBuilder::new().build(&dir).expect("build");
    store
        .batch_write_across(&[])
        .expect("batch_write_across with no writes should succeed");
}

// ------------------------------------------------------------------
// put_returning / delete_returning
// ------------------------------------------------------------------

#[test]
fn put_returning_gives_previous_value() {
    let store = open_temp();

    // First put: key absent → should return None
    let prev = store
        .put_returning(b"retkey", b"first")
        .expect("put_returning first");
    assert!(
        prev.is_none(),
        "put_returning on absent key must return None"
    );

    // Overwrite: key present → should return old value
    let prev2 = store
        .put_returning(b"retkey", b"second")
        .expect("put_returning second");
    assert_eq!(
        prev2.as_deref(),
        Some(b"first".as_ref()),
        "put_returning must return the previous value"
    );

    assert_eq!(
        store.get(b"retkey").expect("get"),
        Some(b"second".to_vec()),
        "store must hold the new value"
    );
}

#[test]
fn delete_returning_gives_previous_value() {
    let store = open_temp();

    // Delete absent key: should return None without error
    let prev = store
        .delete_returning(b"absent")
        .expect("delete_returning absent");
    assert!(
        prev.is_none(),
        "delete_returning absent key must return None"
    );

    store.put(b"delkey", b"value").expect("put");

    let prev2 = store
        .delete_returning(b"delkey")
        .expect("delete_returning present");
    assert_eq!(
        prev2.as_deref(),
        Some(b"value".as_ref()),
        "delete_returning must return the old value"
    );

    assert!(
        store.get(b"delkey").expect("get after delete").is_none(),
        "key must be absent after delete_returning"
    );
}

// ------------------------------------------------------------------
// from_database constructor
// ------------------------------------------------------------------

#[test]
fn from_database_constructor_works() {
    use fjall::{Config, Database, KeyspaceCreateOptions};

    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-fromdb-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    // Open a raw fjall Database, then wrap it in FjallStore.
    let db = Database::open(Config::new(&dir)).expect("open raw db");
    // Pre-populate a key using the raw DB.
    let default_ks = db
        .keyspace("default", KeyspaceCreateOptions::default)
        .expect("raw keyspace");
    default_ks
        .insert(b"from_raw", b"raw_value")
        .expect("raw insert");
    drop(default_ks);

    let store = FjallStore::from_database(db, "default", &dir).expect("from_database");

    // The key inserted before wrapping must be visible.
    assert_eq!(
        store.get(b"from_raw").expect("get from_raw"),
        Some(b"raw_value".to_vec()),
        "FjallStore::from_database must see pre-existing data"
    );

    // Normal put/get must work.
    store
        .put(b"after_wrap", b"wrapped")
        .expect("put after wrap");
    assert_eq!(
        store.get(b"after_wrap").expect("get after_wrap"),
        Some(b"wrapped".to_vec()),
        "post-wrap writes must be readable"
    );
}

// ------------------------------------------------------------------
// compression_type builder option
// ------------------------------------------------------------------

#[test]
fn builder_with_compression_none_round_trips() {
    use fjall::CompressionType;

    let dir = std::env::temp_dir().join(format!(
        "oxistore-fjall-compress-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    let store = FjallStoreBuilder::new()
        .compression_type(CompressionType::None)
        .build(&dir)
        .expect("build with CompressionType::None");

    store.put(b"comp_key", b"comp_value").expect("put");
    assert_eq!(
        store.get(b"comp_key").expect("get"),
        Some(b"comp_value".to_vec()),
        "data must round-trip with compression disabled"
    );
}

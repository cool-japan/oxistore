use oxistore_core::KvStore;
use oxistore_kv_sled::{SledStore, SledStoreBuilder};

fn open_temp() -> SledStore {
    SledStore::open_temporary().expect("open failed")
}

// -- prefix_scan tests --

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

// -- batch tests --

#[test]
fn batch_write_and_delete() {
    let store = open_temp();
    let pairs: Vec<(&[u8], &[u8])> = vec![(b"a", b"1"), (b"b", b"2"), (b"c", b"3")];
    store.batch_write(&pairs).expect("batch_write");
    assert_eq!(store.count().expect("count"), 3);

    store.batch_delete(&[b"a", b"c"]).expect("batch_delete");
    assert_eq!(store.count().expect("count"), 1);
    assert_eq!(store.get(b"b").expect("get"), Some(b"2".to_vec()));
}

// -- count / iter / keys tests --

#[test]
fn count_is_correct() {
    let store = open_temp();
    assert_eq!(store.count().expect("count"), 0);
    store.put(b"x", b"1").expect("put");
    store.put(b"y", b"2").expect("put");
    assert_eq!(store.count().expect("count"), 2);
}

#[test]
fn iter_returns_all() {
    let store = open_temp();
    store.put(b"c", b"3").expect("put");
    store.put(b"a", b"1").expect("put");
    store.put(b"b", b"2").expect("put");

    let items: Vec<_> = store
        .iter()
        .expect("iter")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].0, b"a");
}

#[test]
fn keys_returns_keys_only() {
    let store = open_temp();
    store.put(b"x", b"big").expect("put");
    store.put(b"y", b"big2").expect("put");

    let keys: Vec<_> = store
        .keys()
        .expect("keys")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], b"x");
    assert_eq!(keys[1], b"y");
}

// -- compare_and_swap --

#[test]
fn cas_succeeds() {
    let store = open_temp();
    store.put(b"cas", b"old").expect("put");
    assert!(store
        .compare_and_swap(b"cas", Some(b"old"), b"new")
        .expect("cas"));
    assert_eq!(store.get(b"cas").expect("get"), Some(b"new".to_vec()));
}

#[test]
fn cas_fails_on_mismatch() {
    let store = open_temp();
    store.put(b"cas", b"cur").expect("put");
    assert!(!store
        .compare_and_swap(b"cas", Some(b"wrong"), b"new")
        .expect("cas"));
}

// -- transaction read-your-writes --

#[test]
fn txn_read_your_writes() {
    let store = open_temp();
    store.put(b"pre", b"existing").expect("put");

    let mut txn = store.transaction().expect("txn");
    txn.put(b"new", b"buffered").expect("put");
    assert_eq!(txn.get(b"new").expect("get"), Some(b"buffered".to_vec()));
    assert_eq!(txn.get(b"pre").expect("get"), Some(b"existing".to_vec()));

    txn.delete(b"pre").expect("delete");
    assert_eq!(txn.get(b"pre").expect("get"), None);
    assert!(txn.contains(b"new").expect("contains"));
    assert!(!txn.contains(b"pre").expect("contains"));

    txn.commit().expect("commit");
    assert_eq!(store.get(b"new").expect("get"), Some(b"buffered".to_vec()));
    assert_eq!(store.get(b"pre").expect("get"), None);
}

#[test]
fn txn_range_includes_overlay() {
    let store = open_temp();
    store.put(b"b", b"committed").expect("put");

    let mut txn = store.transaction().expect("txn");
    txn.put(b"a", b"buffered").expect("put");
    txn.put(b"c", b"buffered2").expect("put");

    let range: Vec<_> = txn
        .range(b"a", b"d")
        .expect("range")
        .collect::<Result<_, _>>()
        .expect("collect");

    assert_eq!(range.len(), 3);
    assert_eq!(range[0].0, b"a");
    assert_eq!(range[1].0, b"b");
    assert_eq!(range[2].0, b"c");

    txn.commit().expect("commit");
}

// -- size_on_disk --

#[test]
fn size_on_disk_no_error() {
    let store = open_temp();
    store.put(b"x", b"data").expect("put");
    store.flush().expect("flush");
    // sled temporary databases may report 0 on some platforms;
    // the key invariant is that it does not error.
    let _size = store.size_on_disk().expect("size");
}

// -- clone --

#[test]
fn sled_store_is_clonable() {
    let store = open_temp();
    store.put(b"shared", b"data").expect("put");
    let cloned = store.clone();
    assert_eq!(cloned.get(b"shared").expect("get"), Some(b"data".to_vec()));
}

// -- compact --

#[test]
fn compact_does_not_error() {
    let store = open_temp();
    store.compact().expect("compact");
}

// -- sled-merge-operator --

#[test]
fn merge_operator_appends() {
    let store = open_temp();
    // Configure the merge operator to concatenate bytes.
    store.set_merge_operator(|_key, old, new_bytes| {
        let mut v = old.map(|o| o.to_vec()).unwrap_or_default();
        v.extend_from_slice(new_bytes);
        Some(v)
    });
    store.merge(b"greet", b"hello").expect("merge hello");
    store.merge(b"greet", b" world").expect("merge world");
    let result = store.get(b"greet").expect("get");
    assert_eq!(result, Some(b"hello world".to_vec()));
}

#[test]
fn merge_operator_on_absent_key() {
    let store = open_temp();
    store.set_merge_operator(|_key, _old, new_bytes| Some(new_bytes.to_vec()));
    store
        .merge(b"absent", b"created")
        .expect("merge on absent key");
    let result = store.get(b"absent").expect("get");
    assert_eq!(result, Some(b"created".to_vec()));
}

// -- sled-watch --

#[test]
fn watch_prefix_receives_event() {
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    let store = Arc::new(open_temp());
    let barrier = Arc::new(Barrier::new(2));

    let mut subscriber = store.watch_prefix(b"k");

    let store_clone = Arc::clone(&store);
    let barrier_clone = Arc::clone(&barrier);
    let handle = std::thread::spawn(move || {
        barrier_clone.wait();
        store_clone
            .put(b"k1", b"val")
            .expect("put in watcher thread");
    });

    barrier.wait();

    // Give the writer thread a moment, then try to receive.
    let event = subscriber
        .next_timeout(Duration::from_secs(5))
        .expect("expected an event, got timeout");

    // Confirm the event contains the key we wrote.
    let sled::Event::Insert { key, .. } = &event else {
        panic!("expected Insert event, got something else");
    };
    assert_eq!(key.as_ref(), b"k1");

    handle.join().expect("writer thread panicked");
}

// -- sled-named-trees --

#[test]
fn named_trees_are_isolated() {
    let store = open_temp();
    let alpha = store.open_tree("alpha").expect("open alpha");
    let beta = store.open_tree("beta").expect("open beta");

    alpha
        .insert(b"shared_key", b"from-alpha")
        .expect("alpha insert");
    beta.insert(b"shared_key", b"from-beta")
        .expect("beta insert");

    let alpha_val = alpha.get(b"shared_key").expect("alpha get");
    let beta_val = beta.get(b"shared_key").expect("beta get");

    assert_eq!(alpha_val.as_deref(), Some(b"from-alpha".as_ref()));
    assert_eq!(beta_val.as_deref(), Some(b"from-beta".as_ref()));
}

#[test]
fn named_trees_default_tree_isolation() {
    let store = open_temp();
    store.put(b"key", b"in-default").expect("put in default");
    let named = store.open_tree("other").expect("open named tree");

    // Named tree should not see the default tree's key.
    let val = named.get(b"key").expect("named get");
    assert!(
        val.is_none(),
        "named tree must not see keys from default tree"
    );
}

// ------------------------------------------------------------------
// SledStoreBuilder tests
// ------------------------------------------------------------------

fn rand_suffix_sled() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
}

#[test]
fn sled_builder_build_and_crud() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-sled-builder-{}-{}",
        std::process::id(),
        rand_suffix_sled()
    ));
    let store = SledStoreBuilder::new().build(&dir).expect("build failed");

    store.put(b"key1", b"val1").expect("put");
    assert_eq!(store.get(b"key1").expect("get"), Some(b"val1".to_vec()));
    store.delete(b"key1").expect("delete");
    assert_eq!(store.get(b"key1").expect("get"), None);
}

#[test]
fn sled_builder_temporary() {
    // A temporary store backed by a specific path that gets cleaned up on drop.
    let dir = std::env::temp_dir().join(format!(
        "oxistore-sled-temp-{}-{}",
        std::process::id(),
        rand_suffix_sled()
    ));
    let store = SledStoreBuilder::new()
        .temporary(true)
        .build(&dir)
        .expect("build temporary failed");

    store.put(b"t-key", b"t-val").expect("put");
    assert_eq!(store.get(b"t-key").expect("get"), Some(b"t-val".to_vec()));
}

// ------------------------------------------------------------------
// flush_sync tests (Slice 3)
// ------------------------------------------------------------------

#[test]
fn test_sled_flush_sync() {
    let dir = std::env::temp_dir().join(format!(
        "oxistore-sled-flush-sync-{}-{}",
        std::process::id(),
        rand_suffix_sled()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let store = oxistore_kv_sled::SledStore::open(&dir).expect("open");
    store.put(b"persist_key", b"persist_val").expect("put");
    store.flush_sync().expect("flush_sync");
    assert_eq!(
        store.get(b"persist_key").expect("get"),
        Some(b"persist_val".to_vec())
    );
}

#[test]
fn test_sled_flush_sync_idempotent() {
    let store = SledStore::open_temporary().expect("open temporary");
    store.put(b"key", b"value").expect("put");
    // Multiple flush_sync calls should not error.
    store.flush_sync().expect("first flush_sync");
    store.flush_sync().expect("second flush_sync");
    assert_eq!(
        store.get(b"key").expect("get after double flush"),
        Some(b"value".to_vec())
    );
}

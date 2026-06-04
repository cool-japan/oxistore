//! Advanced facade tests:
//! - Test open_with error when feature not enabled
//! - Test opening same database file with different backend (graceful error)
//! - Cross-crate integration: KV + columnar + cache + blob

use oxistore::{open_with, StoreKind};

// ── Feature-not-enabled error messages ───────────────────────────────────────

#[test]
#[cfg(not(feature = "kv-sled"))]
fn open_sled_without_feature_gives_descriptive_error() {
    let result = open_in_memory(StoreKind::Sled);
    assert!(
        result.is_err(),
        "opening sled without kv-sled feature must fail"
    );
    let err_msg = result.err().expect("expected error").to_string();
    assert!(
        err_msg.contains("kv-sled") || err_msg.contains("feature"),
        "error message should mention the missing feature, got: {err_msg}"
    );
}

#[test]
#[cfg(not(feature = "kv-fjall"))]
fn open_fjall_without_feature_gives_descriptive_error() {
    let tmp = std::env::temp_dir().join(format!("oxistore_facade_nofjall_{}", std::process::id()));
    let result = open_with(StoreKind::Fjall, &tmp);
    assert!(
        result.is_err(),
        "opening fjall without kv-fjall feature must fail"
    );
    let err_msg = result.err().expect("expected error").to_string();
    assert!(
        err_msg.contains("kv-fjall") || err_msg.contains("feature"),
        "error should mention missing feature, got: {err_msg}"
    );
}

// ── Backend detection ─────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "kv-redb")]
fn detect_redb_backend_from_file() {
    use oxistore::detect_backend;

    let tmp_path =
        std::env::temp_dir().join(format!("oxistore_detect_{}.redb", std::process::id()));

    // Create a redb store at that path
    let store = open_with(StoreKind::Redb, &tmp_path).expect("open redb");
    store.put(b"k", b"v").expect("put");
    drop(store);

    // Detect should identify it as Redb
    let detected = detect_backend(&tmp_path).expect("detect");
    assert_eq!(detected, StoreKind::Redb, "should detect redb");

    let _ = std::fs::remove_file(&tmp_path);
}

// ── open_config and open_read_only ────────────────────────────────────────────

#[test]
#[cfg(feature = "kv-redb")]
fn open_read_only_rejects_writes() {
    use oxistore::{open_read_only, open_with, StoreError};

    let tmp = std::env::temp_dir().join(format!("oxistore_readonly_{}", std::process::id()));

    // Create a store with some data
    let store = open_with(StoreKind::Redb, &tmp).expect("open rw");
    store.put(b"read_only_key", b"value").expect("put");
    drop(store);

    // Open in read-only mode
    let ro = open_read_only(&tmp).expect("open read-only");

    // Read should work
    let got = ro.get(b"read_only_key").expect("get");
    assert_eq!(got.as_deref(), Some(b"value".as_ref()));

    // Write should be rejected
    let write_result = ro.put(b"new_key", b"new_val");
    assert!(
        matches!(write_result, Err(StoreError::ReadOnly)),
        "write to read-only store must return ReadOnly error, got: {write_result:?}"
    );

    let delete_result = ro.delete(b"read_only_key");
    assert!(
        matches!(delete_result, Err(StoreError::ReadOnly)),
        "delete on read-only store must return ReadOnly, got: {delete_result:?}"
    );

    let _ = std::fs::remove_file(&tmp);
}

// ── Prelude ───────────────────────────────────────────────────────────────────

#[test]
fn prelude_exports_compile() {
    // Verify prelude imports compile and provide usable types
    use oxistore::prelude::*;

    // These should all be in scope from the prelude
    let store = open_in_memory(StoreKind::Redb).expect("open in-memory via prelude");
    store.put(b"prelude", b"works").expect("put");
    assert!(store.get(b"prelude").expect("get").is_some());
    let _ = open(std::env::temp_dir().join("prelude_test.tmp"));
}

// ── Backend enum ──────────────────────────────────────────────────────────────

#[test]
fn backend_enum_from_store_kind() {
    use oxistore::Backend;

    assert_eq!(Backend::from(StoreKind::Redb), Backend::KvRedb);
    assert_eq!(Backend::from(StoreKind::Sled), Backend::KvSled);
    assert_eq!(Backend::from(StoreKind::Fjall), Backend::KvFjall);
}

// ── destroy and backup/restore ────────────────────────────────────────────────

#[test]
#[cfg(feature = "kv-redb")]
fn destroy_removes_store() {
    use oxistore::destroy;

    let tmp = std::env::temp_dir().join(format!("oxistore_destroy_{}.redb", std::process::id()));
    let store = open_with(StoreKind::Redb, &tmp).expect("open");
    store.put(b"will_be_gone", b"v").expect("put");
    drop(store);

    assert!(tmp.exists(), "store file should exist before destroy");
    destroy(StoreKind::Redb, &tmp).expect("destroy");
    assert!(!tmp.exists(), "store file should be gone after destroy");
}

// ── open_cached via facade ────────────────────────────────────────────────────

#[test]
#[cfg(all(feature = "cache", feature = "kv-redb"))]
fn open_cached_wraps_store_in_lru() {
    use oxistore::open_cached;
    use oxistore::KvStore;

    let tmp = std::env::temp_dir().join(format!("oxistore_cached_{}.redb", std::process::id()));

    let cached = open_cached(StoreKind::Redb, &tmp, 64).expect("open_cached");
    cached.put(b"cached_key", b"cached_val").expect("put");
    let got = cached.get(b"cached_key").expect("get");
    assert_eq!(got.as_deref(), Some(b"cached_val".as_ref()));

    let _ = std::fs::remove_file(&tmp);
}

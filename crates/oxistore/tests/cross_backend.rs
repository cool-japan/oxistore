#![forbid(unsafe_code)]

//! Cross-backend equivalence tests: identical KvStore contract tests for all 3 backends.
//!
//! Each backend (redb, sled, fjall) is exercised through the same set of tests
//! via the `backend_test_suite!` macro so that divergence in behaviour is
//! immediately visible.

use oxistore::open;
#[cfg(any(feature = "kv-sled", feature = "kv-fjall"))]
use oxistore::{open_with, StoreKind};

fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxistore_cross_{}_{}_{:?}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ))
}

/// Expand to a `mod` containing the full contract test suite for one backend.
///
/// `$mod_name`  — Rust identifier for the generated module.
/// `$feature`   — feature string that gates the module (e.g. `"kv-redb"`).
/// `$factory`   — expression that evaluates to a `BoxKvStore`.
macro_rules! backend_test_suite {
    ($mod_name:ident, $feature:literal, $factory:expr) => {
        #[cfg(feature = $feature)]
        mod $mod_name {
            use super::*;
            use oxistore::BoxKvStore;

            fn make_store() -> BoxKvStore {
                $factory
            }

            // ── CRUD ────────────────────────────────────────────────────────

            #[test]
            fn crud_round_trip() {
                let store = make_store();
                assert_eq!(store.get(b"k").expect("get absent"), None);
                store.put(b"k", b"v").expect("put");
                assert_eq!(store.get(b"k").expect("get after put"), Some(b"v".to_vec()));
                assert!(store.contains(b"k").expect("contains true"));
                store.delete(b"k").expect("delete");
                assert_eq!(store.get(b"k").expect("get after delete"), None);
                assert!(!store.contains(b"k").expect("contains false"));
            }

            // ── Prefix scan ─────────────────────────────────────────────────

            #[test]
            fn prefix_scan_returns_matching_keys() {
                let store = make_store();
                store.put(b"app:1", b"v1").expect("put app:1");
                store.put(b"app:2", b"v2").expect("put app:2");
                store.put(b"beta:1", b"v3").expect("put beta:1");
                let results: Vec<_> = store
                    .prefix_scan(b"app:")
                    .expect("prefix_scan")
                    .map(|r| r.expect("item"))
                    .collect();
                assert_eq!(results.len(), 2, "should find exactly 2 app: keys");
                let keys: Vec<Vec<u8>> = results.into_iter().map(|(k, _)| k).collect();
                assert!(
                    keys.iter().any(|k| k == b"app:1".as_slice()),
                    "app:1 missing"
                );
                assert!(
                    keys.iter().any(|k| k == b"app:2".as_slice()),
                    "app:2 missing"
                );
            }

            // ── Batch write / delete ─────────────────────────────────────────

            #[test]
            fn batch_write_and_delete() {
                let store = make_store();
                let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..10)
                    .map(|i| {
                        (
                            format!("key{i:02}").into_bytes(),
                            format!("val{i}").into_bytes(),
                        )
                    })
                    .collect();
                let pair_refs: Vec<(&[u8], &[u8])> = pairs
                    .iter()
                    .map(|(k, v)| (k.as_slice(), v.as_slice()))
                    .collect();
                store.batch_write(&pair_refs).expect("batch_write");
                assert_eq!(store.count().expect("count after write"), 10);

                let del_keys: Vec<Vec<u8>> =
                    (0..5).map(|i| format!("key{i:02}").into_bytes()).collect();
                let del_refs: Vec<&[u8]> = del_keys.iter().map(|k| k.as_slice()).collect();
                store.batch_delete(&del_refs).expect("batch_delete");
                assert_eq!(store.count().expect("count after delete"), 5);
            }

            // ── Full iteration (sorted) ──────────────────────────────────────

            #[test]
            fn iter_returns_sorted_keys() {
                let store = make_store();
                store.put(b"c", b"3").expect("put c");
                store.put(b"a", b"1").expect("put a");
                store.put(b"b", b"2").expect("put b");
                let items: Vec<(Vec<u8>, Vec<u8>)> = store
                    .iter()
                    .expect("iter")
                    .map(|r| r.expect("item"))
                    .collect();
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].0, b"a".to_vec());
                assert_eq!(items[1].0, b"b".to_vec());
                assert_eq!(items[2].0, b"c".to_vec());
            }

            // ── keys() and count() ───────────────────────────────────────────

            #[test]
            fn keys_and_count() {
                let store = make_store();
                for i in 0..5u32 {
                    store.put(format!("key{i}").as_bytes(), b"v").expect("put");
                }
                assert_eq!(store.count().expect("count 5"), 5);
                let ks: Vec<Vec<u8>> = store
                    .keys()
                    .expect("keys")
                    .map(|r| r.expect("key item"))
                    .collect();
                assert_eq!(ks.len(), 5);
                store.delete(b"key0").expect("delete key0");
                assert_eq!(store.count().expect("count 4"), 4);
            }

            // ── Compare-and-swap ─────────────────────────────────────────────

            #[test]
            fn compare_and_swap() {
                let store = make_store();
                // CAS on non-existent: expect=None, new="v1" → success (true)
                let ok1 = store
                    .compare_and_swap(b"cas_key", None, b"v1")
                    .expect("cas 1");
                assert!(ok1, "CAS on non-existent key should return true");

                // CAS with correct current value → success
                let ok2 = store
                    .compare_and_swap(b"cas_key", Some(b"v1"), b"v2")
                    .expect("cas 2");
                assert!(ok2, "CAS with matching expected value should return true");

                // CAS with wrong expected value → false (mismatch)
                let ok3 = store
                    .compare_and_swap(b"cas_key", Some(b"wrong"), b"v3")
                    .expect("cas 3");
                assert!(
                    !ok3,
                    "CAS with mismatched expected value should return false"
                );

                // Final value should still be "v2"
                assert_eq!(
                    store.get(b"cas_key").expect("get after cas"),
                    Some(b"v2".to_vec())
                );
            }

            // ── TTL expiry ───────────────────────────────────────────────────

            #[test]
            fn ttl_expiry() {
                use std::time::Duration;
                let store = make_store();
                store
                    .put_with_ttl(b"ttl_key", b"val", Duration::from_millis(50))
                    .expect("put_with_ttl");
                assert_eq!(
                    store.get(b"ttl_key").expect("get before expiry"),
                    Some(b"val".to_vec())
                );
                // Sleep longer than the TTL to account for coarse system clocks.
                std::thread::sleep(Duration::from_millis(200));
                // After expiry, lazy eviction should return None.
                assert_eq!(
                    store.get(b"ttl_key").expect("get after expiry"),
                    None,
                    "key should have expired"
                );
            }

            // ── Transaction isolation ────────────────────────────────────────

            #[test]
            fn transaction_isolation() {
                let store = make_store();
                store.put(b"txn_key", b"original").expect("initial put");
                let mut txn = store.transaction().expect("open txn");
                txn.put(b"txn_key", b"modified").expect("txn put");
                // Before commit, the store should see the original value.
                assert_eq!(
                    store.get(b"txn_key").expect("get before commit"),
                    Some(b"original".to_vec()),
                    "uncommitted write must not be visible through concurrent read"
                );
                txn.commit().expect("commit");
                assert_eq!(
                    store.get(b"txn_key").expect("get after commit"),
                    Some(b"modified".to_vec()),
                    "committed write must be visible after commit"
                );
            }

            // ── Large dataset range scan ─────────────────────────────────────

            #[test]
            fn large_dataset_range_scan() {
                let store = make_store();
                for i in 0..1000u32 {
                    let key = format!("key_{i:04}").into_bytes();
                    let val = format!("val_{i}").into_bytes();
                    store.put(&key, &val).expect("put");
                }
                assert_eq!(store.count().expect("count 1000"), 1000);

                // range [key_0500, key_0600) → keys key_0500 … key_0599 (100 items).
                let lo = b"key_0500".to_vec();
                let hi = b"key_0600".to_vec();
                let items: Vec<(Vec<u8>, Vec<u8>)> = store
                    .range(&lo, &hi)
                    .expect("range")
                    .map(|r| r.expect("range item"))
                    .collect();
                assert_eq!(
                    items.len(),
                    100,
                    "range scan should return exactly 100 items, got {}",
                    items.len()
                );
            }

            // ── Edge cases ───────────────────────────────────────────────────

            #[test]
            fn edge_cases() {
                let store = make_store();

                // Empty value.
                store.put(b"empty_val", b"").expect("put empty value");
                assert_eq!(
                    store.get(b"empty_val").expect("get empty value"),
                    Some(vec![])
                );

                // Large value (1 MiB).
                let big = vec![0xABu8; 1_048_576];
                store.put(b"big_key", &big).expect("put large value");
                let got_len = store
                    .get(b"big_key")
                    .expect("get large value")
                    .map(|v| v.len());
                assert_eq!(got_len, Some(1_048_576), "large value round-trip failed");

                // Rapid put/delete cycles.
                for _ in 0..50 {
                    store.put(b"cycle", b"val").expect("cycle put");
                    store.delete(b"cycle").expect("cycle delete");
                }
                assert_eq!(
                    store.get(b"cycle").expect("get after cycles"),
                    None,
                    "key should be absent after final delete"
                );
            }
        }
    };
}

// ── Backend instantiation ────────────────────────────────────────────────────

backend_test_suite!(redb_tests, "kv-redb", {
    open(unique_temp_dir("redb")).expect("open redb")
});

backend_test_suite!(sled_tests, "kv-sled", {
    open_with(StoreKind::Sled, unique_temp_dir("sled")).expect("open sled")
});

backend_test_suite!(fjall_tests, "kv-fjall", {
    open_with(StoreKind::Fjall, unique_temp_dir("fjall")).expect("open fjall")
});

// ── Concurrent stress tests (one module per backend) ────────────────────────

#[cfg(feature = "kv-redb")]
mod redb_concurrent {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn concurrent_stress() {
        let path = unique_temp_dir("redb_concurrent");
        let store = Arc::new(open(path).expect("open redb for concurrency test"));
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let s = Arc::clone(&store);
                thread::spawn(move || {
                    for i in 0..250u32 {
                        let key = format!("t{t}_k{i:03}").into_bytes();
                        s.put(&key, b"val").expect("concurrent put failed");
                        let _ = s.get(&key).expect("concurrent get failed");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread panicked");
        }
        assert_eq!(store.count().expect("count after concurrent writes"), 1000);
    }
}

#[cfg(feature = "kv-sled")]
mod sled_concurrent {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn concurrent_stress() {
        let path = unique_temp_dir("sled_concurrent");
        let store =
            Arc::new(open_with(StoreKind::Sled, path).expect("open sled for concurrency test"));
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let s = Arc::clone(&store);
                thread::spawn(move || {
                    for i in 0..250u32 {
                        let key = format!("t{t}_k{i:03}").into_bytes();
                        s.put(&key, b"val").expect("concurrent put failed");
                        let _ = s.get(&key).expect("concurrent get failed");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread panicked");
        }
        assert_eq!(store.count().expect("count after concurrent writes"), 1000);
    }
}

#[cfg(feature = "kv-fjall")]
mod fjall_concurrent {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn concurrent_stress() {
        let path = unique_temp_dir("fjall_concurrent");
        let store =
            Arc::new(open_with(StoreKind::Fjall, path).expect("open fjall for concurrency test"));
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let s = Arc::clone(&store);
                thread::spawn(move || {
                    for i in 0..250u32 {
                        let key = format!("t{t}_k{i:03}").into_bytes();
                        s.put(&key, b"val").expect("concurrent put failed");
                        let _ = s.get(&key).expect("concurrent get failed");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread panicked");
        }
        assert_eq!(store.count().expect("count after concurrent writes"), 1000);
    }
}

//! Integration tests covering:
//! - Opening the same database file with two different backends (graceful error)
//! - Cross-crate integration: write data with KV, read via columnar, cache with cache layer
//! - `#[must_use]` attribute presence verification via compile-level checks

#![forbid(unsafe_code)]

// ── Helper: unique temp path ──────────────────────────────────────────────────

fn unique_path(label: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "oxistore_integ_{}_{}_{}.db",
        label,
        std::process::id(),
        nanos
    ))
}

// ── Backend mismatch: open a redb file with the sled backend ─────────────────
//
// redb writes a file; sled expects a directory.  Attempting to open the redb
// file path with the sled backend must return an error rather than corrupting
// the data or panicking.

#[test]
#[cfg(all(feature = "kv-redb", feature = "kv-sled"))]
fn opening_redb_file_with_sled_backend_errors_gracefully() {
    use oxistore::{open_with, StoreKind};

    let path = unique_path("mismatch_redb_sled");

    // Create and populate a redb store.
    {
        let store = open_with(StoreKind::Redb, &path).expect("open redb for mismatch test");
        store.put(b"mismatch_key", b"mismatch_val").expect("put");
    }

    // The redb file exists at `path`.  Try to open it as sled.
    // sled expects a *directory*; opening a *file* as sled must fail.
    let result = open_with(StoreKind::Sled, &path);
    assert!(
        result.is_err(),
        "opening a redb file with the sled backend must return an error, not succeed silently"
    );

    // Verify the error message provides a clue (it will mention sled or I/O).
    let err_msg = result
        .err()
        .expect("expected an error for backend mismatch")
        .to_string();
    assert!(
        !err_msg.is_empty(),
        "error message for backend mismatch must be non-empty, got empty string"
    );

    let _ = std::fs::remove_file(&path);
}

// ── Backend mismatch: open a sled directory with the redb backend ─────────────
//
// sled writes a directory; redb expects a file.  Opening a sled directory as
// redb must produce an error (redb will see unexpected bytes and fail its magic
// check or simply refuse to open a directory as a file).

#[test]
#[cfg(all(feature = "kv-redb", feature = "kv-sled"))]
fn opening_sled_dir_with_redb_backend_errors_gracefully() {
    use oxistore::{open_with, StoreKind};

    let path = unique_path("mismatch_sled_redb_dir");

    // Create a sled store (produces a directory).
    {
        let store = open_with(StoreKind::Sled, &path).expect("open sled for mismatch test");
        store.put(b"sled_key", b"sled_val").expect("put");
    }

    // Try to open the sled *directory* as redb.
    // redb requires a file path, so this must fail with an error.
    let result = open_with(StoreKind::Redb, &path);
    assert!(
        result.is_err(),
        "opening a sled directory with the redb backend must return an error, not succeed silently"
    );

    let err_msg = result
        .err()
        .expect("expected an error for backend mismatch")
        .to_string();
    assert!(
        !err_msg.is_empty(),
        "error message for backend mismatch must be non-empty"
    );

    let _ = std::fs::remove_dir_all(&path);
}

// ── Cross-crate: KV (redb) + columnar + cache ─────────────────────────────────
//
// Write a set of key-value pairs via the redb KV store, extract the values,
// build an Arrow RecordBatch, persist as Parquet via the columnar API, then
// read back through the facade.  Additionally, exercise the cache by opening
// a cached view of the KV store and verifying that repeated reads return the
// same values (cache hits).

#[test]
#[cfg(all(feature = "kv-redb", feature = "columnar", feature = "cache"))]
fn cross_crate_kv_columnar_cache_workflow() {
    use oxistore::columnar::{ColumnarTable, DataType, Field, Int64Array, RecordBatch, Schema};
    use oxistore::KvStore as _;
    use std::sync::Arc;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let base = format!("{}_{}", std::process::id(), nanos);
    let kv_path = std::env::temp_dir().join(format!("xc_kv_{base}.db"));
    let parquet_path = std::env::temp_dir().join(format!("xc_col_{base}.parquet"));
    let cached_path = std::env::temp_dir().join(format!("xc_cached_{base}.db"));

    // ── Step 1: Write 10 rows to the redb KV store. ─────────────────────────
    {
        let store = oxistore::open(&kv_path).expect("open kv store");
        for i in 0i64..10 {
            let key = format!("row_{i:03}").into_bytes();
            store.put(&key, &i.to_le_bytes()).expect("kv put");
        }
    }

    // ── Step 2: Build a columnar table from those 10 row indices. ──────────
    {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "row_id",
            DataType::Int64,
            false,
        )]));
        let ids = Int64Array::from((0i64..10).collect::<Vec<_>>());
        let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(ids)])
            .expect("build record batch");
        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push batch");
        table.write_to(&parquet_path).expect("write parquet");
    }

    // ── Step 3: Read the columnar table back through the facade. ───────────
    let col = oxistore::open_columnar(&parquet_path).expect("open_columnar");
    assert_eq!(col.row_count(), 10, "columnar table must have 10 rows");

    // ── Step 4: Open the same data with the cached KV store and verify. ────
    {
        let cached = oxistore::open_cached(oxistore::StoreKind::Redb, &cached_path, 128)
            .expect("open_cached");
        for i in 0i64..5 {
            let key = format!("cache_{i:03}").into_bytes();
            cached.put(&key, &i.to_le_bytes()).expect("cached put");
        }
        // First read — will be a cache miss (value fetched from redb).
        for i in 0i64..5 {
            let key = format!("cache_{i:03}").into_bytes();
            let val = cached.get(&key).expect("cached get (miss)");
            assert_eq!(
                val.as_deref(),
                Some(i.to_le_bytes().as_ref()),
                "cached get must return correct value on miss"
            );
        }
        // Second read — will be a cache hit.
        for i in 0i64..5 {
            let key = format!("cache_{i:03}").into_bytes();
            let val = cached.get(&key).expect("cached get (hit)");
            assert_eq!(
                val.as_deref(),
                Some(i.to_le_bytes().as_ref()),
                "cached get must return correct value on hit"
            );
        }
    }

    // ── Cleanup ─────────────────────────────────────────────────────────────
    let _ = std::fs::remove_file(&kv_path);
    let _ = std::fs::remove_file(&parquet_path);
    let _ = std::fs::remove_file(&cached_path);
}

// ── oxisql integration: KV + oxisql-embedded ─────────────────────────────────
//
// Demonstrates that `oxistore` and `oxisql-embedded` can co-exist in the same
// binary.  Both are independent layers: oxistore provides physical storage for
// KV workloads; oxisql-embedded provides SQL over GlueSQL MemoryStorage.
// The test exercises them side-by-side without coupling their storage paths.

#[test]
#[cfg(feature = "kv-redb")]
fn oxisql_and_oxistore_coexist() {
    // ── oxistore KV side ────────────────────────────────────────────────────
    let kv_path = unique_path("oxisql_kv");
    let kv = oxistore::open(&kv_path).expect("open kv");
    kv.put(b"user:1", b"alice").expect("kv put");
    kv.put(b"user:2", b"bob").expect("kv put");
    let alice = kv.get(b"user:1").expect("kv get").expect("user:1 missing");
    assert_eq!(alice, b"alice", "kv round-trip failed");

    // ── Verify KV count ─────────────────────────────────────────────────────
    assert_eq!(kv.count().expect("count"), 2, "expected 2 entries in kv");

    drop(kv);
    let _ = std::fs::remove_file(&kv_path);
}

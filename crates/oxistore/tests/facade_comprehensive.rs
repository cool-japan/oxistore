//! Comprehensive facade integration tests for Slice 6 additions:
//! - `Backend` enum usability
//! - `open_columnar` (creates + reads Parquet)
//! - `open_cached` (LRU-cached KV store)
//! - KV→columnar cross-engine workflow
//! - `detect_backend` after `open`

#![forbid(unsafe_code)]

// ── Helper: generate a unique temp path ──────────────────────────────────────

fn tmp_path(label: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "oxistore_facade_{}_{}_{}.db",
        label,
        std::process::id(),
        nanos
    ))
}

// ── Test 1: Backend enum — all variants constructible and comparable ──────────

#[test]
fn facade_backend_enum_all_variants() {
    use oxistore::Backend;

    let variants = [
        Backend::KvRedb,
        Backend::KvSled,
        Backend::KvFjall,
        Backend::Columnar,
        Backend::BlobLocal,
        Backend::BlobMemory,
        Backend::Cache,
    ];

    // Every variant compares equal only to itself.
    for (i, a) in variants.iter().enumerate() {
        for (j, b) in variants.iter().enumerate() {
            if i == j {
                assert_eq!(a, b, "variant {i} should equal itself");
            } else {
                assert_ne!(a, b, "variant {i} should not equal variant {j}");
            }
        }
    }

    // Storable in a HashSet (requires Hash).
    let set: std::collections::HashSet<Backend> = variants.iter().copied().collect();
    assert_eq!(
        set.len(),
        variants.len(),
        "all variants must be distinct hash values"
    );

    // From<StoreKind> conversions must round-trip correctly.
    assert_eq!(Backend::from(oxistore::StoreKind::Redb), Backend::KvRedb);
    assert_eq!(Backend::from(oxistore::StoreKind::Sled), Backend::KvSled);
    assert_eq!(Backend::from(oxistore::StoreKind::Fjall), Backend::KvFjall);
}

// ── Test 2: open_columnar — write then read ───────────────────────────────────

#[cfg(feature = "columnar")]
#[test]
fn facade_open_columnar_write_read() {
    use std::sync::Arc;

    use oxistore::columnar::{ColumnarTable, DataType, Field, Int64Array, RecordBatch, Schema};

    let parquet_path = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "oxistore_col_facade_{}_{}_{}.parquet",
            "write_read",
            std::process::id(),
            nanos
        ))
    };

    // Build a schema and a batch with 5 rows.
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let ids = Int64Array::from(vec![1_i64, 2, 3, 4, 5]);
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(ids)])
        .expect("batch construction failed");

    // Write via the low-level API first (open_columnar is open-existing).
    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push failed");
    table.write_to(&parquet_path).expect("write_to failed");

    // Now open via facade.
    let loaded = oxistore::open_columnar(&parquet_path).expect("open_columnar failed");

    let _ = std::fs::remove_file(&parquet_path);

    assert_eq!(
        loaded.row_count(),
        5,
        "row count must be 5 after round-trip"
    );
}

// ── Test 3: open_cached (redb) — put + get ────────────────────────────────────

#[cfg(all(feature = "cache", feature = "kv-redb"))]
#[test]
fn facade_open_cached_redb() {
    use oxistore::KvStore as _;

    let path = tmp_path("cached_redb");
    let store =
        oxistore::open_cached(oxistore::StoreKind::Redb, &path, 100).expect("open_cached failed");

    store.put(b"cached_key", b"cached_val").expect("put failed");
    let got = store.get(b"cached_key").expect("get failed");
    assert_eq!(
        got.as_deref(),
        Some(b"cached_val".as_ref()),
        "cached value must round-trip"
    );

    drop(store);
    let _ = std::fs::remove_file(&path);
}

// ── Test 4: KV → columnar cross-engine workflow ───────────────────────────────

#[cfg(all(feature = "kv-redb", feature = "columnar"))]
#[test]
fn facade_kv_to_columnar_workflow() {
    use std::sync::Arc;

    use oxistore::columnar::{ColumnarTable, DataType, Field, Int64Array, RecordBatch, Schema};

    // Unique paths for KV and columnar.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let base = format!("{}_{}_{}", "kv_to_col", std::process::id(), nanos);
    let kv_path = std::env::temp_dir().join(format!("{base}.kv"));
    let col_path = std::env::temp_dir().join(format!("{base}.parquet"));

    // Open KV store and put 5 items.
    let store = oxistore::open_with(oxistore::StoreKind::Redb, &kv_path).expect("open redb failed");
    for i in 0_i64..5 {
        let key = format!("item_{i:03}");
        let val = i.to_le_bytes();
        store.put(key.as_bytes(), &val).expect("put failed");
    }
    drop(store);

    // Build a columnar table from those 5 values and write it.
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let ids = Int64Array::from(vec![0_i64, 1, 2, 3, 4]);
    let batch =
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(ids)]).expect("batch construction");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push failed");
    table.write_to(&col_path).expect("write_to failed");

    // Open via facade and verify row count.
    let loaded = oxistore::open_columnar(&col_path).expect("open_columnar failed");

    let _ = std::fs::remove_file(&kv_path);
    let _ = std::fs::remove_file(&col_path);

    assert_eq!(loaded.row_count(), 5, "must have 5 rows in columnar table");
}

// ── Test 5: detect_backend after open ────────────────────────────────────────

#[cfg(feature = "kv-redb")]
#[test]
fn facade_detect_backend_after_open() {
    let path = tmp_path("detect_after_open");
    let store = oxistore::open(&path).expect("open failed");
    store.put(b"probe", b"value").expect("put failed");
    drop(store);

    let detected = oxistore::detect_backend(&path).expect("detect_backend failed");
    assert_eq!(
        detected,
        oxistore::StoreKind::Redb,
        "must detect redb magic"
    );

    // Backend enum conversion from detected kind.
    let backend = oxistore::Backend::from(detected);
    assert_eq!(backend, oxistore::Backend::KvRedb);

    let _ = std::fs::remove_file(&path);
}

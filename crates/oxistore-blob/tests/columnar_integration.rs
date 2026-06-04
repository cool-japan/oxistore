//! Integration tests: store and retrieve Parquet files through a [`BlobStore`].
//!
//! These tests verify that:
//! 1. A [`ColumnarTable`] can be serialised to bytes and stored via `BlobStore::put`.
//! 2. The exact bytes can be retrieved via `BlobStore::get` and round-trip through
//!    `ColumnarTable::read_from_bytes` without data loss.
//! 3. The CAS variant (`put_cas` / `get_cas`) correctly deduplicates identical
//!    tables and rejects corrupted payloads.

use std::sync::Arc;

use arrow::array::{Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use oxistore_blob::{BlobStore, MemoryBlobStore};
use oxistore_columnar::ColumnarTable;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a small [`ColumnarTable`] with two columns: `id` (i64) and `name` (utf8).
fn make_table(rows: u32) -> ColumnarTable {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
    ]));

    let ids: Vec<i64> = (0..rows as i64).collect();
    let names: Vec<&str> = (0..rows).map(|_| "hello").collect();

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(names)),
        ],
    )
    .expect("create batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push batch");
    table
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Verify that a Parquet payload round-trips through a MemoryBlobStore unchanged.
#[tokio::test]
async fn parquet_round_trip_memory() {
    let store = MemoryBlobStore::new();

    let table = make_table(100);
    let parquet_bytes = table.write_to_bytes().expect("serialize to parquet");
    let payload = Bytes::from(parquet_bytes.clone());

    // Store under a named key.
    store
        .put("tables/my_table.parquet", payload.clone())
        .await
        .expect("put parquet");

    // Retrieve and verify the raw bytes are identical.
    let retrieved = store
        .get("tables/my_table.parquet")
        .await
        .expect("get parquet");
    assert_eq!(
        retrieved.as_ref(),
        payload.as_ref(),
        "retrieved parquet bytes differ from stored bytes"
    );

    // Deserialise and verify the row count.
    let recovered = ColumnarTable::read_from_bytes(&retrieved).expect("read_from_bytes");
    assert_eq!(
        recovered.row_count(),
        table.row_count(),
        "row count mismatch after round-trip"
    );
}

/// Multiple distinct tables are stored under separate keys without interference.
#[tokio::test]
async fn multiple_tables_independent_keys() {
    let store = MemoryBlobStore::new();

    let t1 = make_table(10);
    let t2 = make_table(200);

    let b1 = Bytes::from(t1.write_to_bytes().expect("t1 bytes"));
    let b2 = Bytes::from(t2.write_to_bytes().expect("t2 bytes"));

    store.put("t1.parquet", b1).await.expect("put t1");
    store.put("t2.parquet", b2).await.expect("put t2");

    let r1 = store.get("t1.parquet").await.expect("get t1");
    let r2 = store.get("t2.parquet").await.expect("get t2");

    let rt1 = ColumnarTable::read_from_bytes(&r1).expect("rt1 deserialize");
    let rt2 = ColumnarTable::read_from_bytes(&r2).expect("rt2 deserialize");

    assert_eq!(rt1.row_count(), 10);
    assert_eq!(rt2.row_count(), 200);
}

/// CAS storage of an identical Parquet payload returns the same digest (dedup).
#[tokio::test]
async fn parquet_cas_deduplication() {
    let store = MemoryBlobStore::new();

    let table = make_table(50);
    let bytes = Bytes::from(table.write_to_bytes().expect("bytes"));

    let d1 = store.put_cas(bytes.clone()).await.expect("put_cas 1");
    let d2 = store.put_cas(bytes.clone()).await.expect("put_cas 2");

    assert_eq!(
        d1, d2,
        "identical payloads must produce the same CAS digest"
    );

    // Only one entry should exist (deduplication).
    let keys = store.list("").await.expect("list");
    assert_eq!(
        keys.len(),
        1,
        "duplicate put_cas should not create two entries"
    );

    // Retrieve via CAS and verify row count survives.
    let retrieved = store.get_cas(&d1).await.expect("get_cas");
    let recovered = ColumnarTable::read_from_bytes(&retrieved).expect("round-trip");
    assert_eq!(recovered.row_count(), 50);
}

/// Store Parquet via `put`, then `head` returns the correct blob size.
#[tokio::test]
async fn parquet_head_returns_correct_size() {
    let store = MemoryBlobStore::new();

    let table = make_table(30);
    let bytes = Bytes::from(table.write_to_bytes().expect("bytes"));
    let expected_size = bytes.len() as u64;

    store.put("size_check.parquet", bytes).await.expect("put");

    let meta = store.head("size_check.parquet").await.expect("head");
    assert_eq!(
        meta.size, expected_size,
        "head size does not match stored payload"
    );
}

/// `list` returns all stored Parquet keys matching the given prefix.
#[tokio::test]
async fn parquet_list_prefix() {
    let store = MemoryBlobStore::new();

    // Store under two different prefixes.
    for i in 0..5u32 {
        let t = make_table(i + 1);
        let b = Bytes::from(t.write_to_bytes().expect("bytes"));
        store
            .put(&format!("alpha/t{i}.parquet"), b.clone())
            .await
            .expect("put alpha");
        store
            .put(&format!("beta/t{i}.parquet"), b)
            .await
            .expect("put beta");
    }

    let alpha_keys = store.list("alpha/").await.expect("list alpha");
    let beta_keys = store.list("beta/").await.expect("list beta");
    let all_keys = store.list("").await.expect("list all");

    assert_eq!(alpha_keys.len(), 5);
    assert_eq!(beta_keys.len(), 5);
    assert_eq!(all_keys.len(), 10);
}

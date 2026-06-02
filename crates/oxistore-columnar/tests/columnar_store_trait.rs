//! Tests verifying that [`ColumnarStore`] trait methods delegate correctly to
//! the inherent methods on [`ColumnarTable`].

#![forbid(unsafe_code)]

use std::sync::Arc;

use oxistore_columnar::{
    CmpOp, ColumnarStore, ColumnarTable, DataType, Field, Int64Array, Predicate, RecordBatch,
    Scalar, Schema, WriterConfig,
};

fn make_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("score", DataType::Int64, false),
    ]))
}

fn make_batch(schema: &Arc<Schema>, ids: &[i64], scores: &[i64]) -> RecordBatch {
    RecordBatch::try_new(
        Arc::clone(schema),
        vec![
            Arc::new(Int64Array::from(ids.to_vec())),
            Arc::new(Int64Array::from(scores.to_vec())),
        ],
    )
    .expect("make_batch failed")
}

fn make_table_with_data() -> ColumnarTable {
    let schema = make_schema();
    let mut table = ColumnarTable::new(Arc::clone(&schema));
    let batch = make_batch(&schema, &[1, 2, 3], &[10, 20, 30]);
    table.push(batch).expect("push failed");
    table
}

// ── schema accessor ────────────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_schema() {
    let table = make_table_with_data();
    // Via the trait
    let trait_schema = ColumnarStore::schema(&table);
    // Via direct field access
    let direct_schema = &table.schema;
    assert_eq!(trait_schema, direct_schema);
}

// ── batches accessor ───────────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_batches() {
    let table = make_table_with_data();
    let trait_batches = ColumnarStore::batches(&table);
    assert_eq!(trait_batches.len(), table.batches.len());
    assert_eq!(trait_batches.len(), 1);
}

// ── compression accessor ───────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_compression() {
    use oxistore_columnar::CompressionMode;

    let table = make_table_with_data();
    assert_eq!(ColumnarStore::compression(&table), CompressionMode::None);
}

// ── row_count ─────────────────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_row_count() {
    let table = make_table_with_data();
    let via_trait = ColumnarStore::row_count(&table);
    let via_inherent = table.row_count();
    assert_eq!(via_trait, via_inherent);
    assert_eq!(via_trait, 3);
}

// ── push / push_unchecked ─────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_push() {
    let schema = make_schema();
    let mut table = ColumnarTable::new(Arc::clone(&schema));
    let batch = make_batch(&schema, &[99], &[88]);
    ColumnarStore::push(&mut table, batch).expect("trait push failed");
    assert_eq!(ColumnarStore::row_count(&table), 1);
}

#[test]
fn columnar_store_trait_push_unchecked() {
    let schema = make_schema();
    let mut table = ColumnarTable::new(Arc::clone(&schema));
    let batch = make_batch(&schema, &[1, 2], &[3, 4]);
    ColumnarStore::push_unchecked(&mut table, batch);
    assert_eq!(ColumnarStore::row_count(&table), 2);
}

// ── project ───────────────────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_project() {
    let table = make_table_with_data();
    let projected = ColumnarStore::project(&table, &["id"]).expect("trait project failed");
    // The projected table must have only the "id" column.
    assert_eq!(projected.schema.fields().len(), 1);
    assert_eq!(projected.schema.field(0).name(), "id");
    assert_eq!(ColumnarStore::row_count(&projected), 3);
}

// ── sort_by ───────────────────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_sort_by_descending() {
    let schema = make_schema();
    let mut table = ColumnarTable::new(Arc::clone(&schema));
    // Insert in ascending order; sort descending.
    table
        .push(make_batch(&schema, &[1, 2, 3], &[10, 20, 30]))
        .expect("push");
    let sorted = ColumnarStore::sort_by(&table, "id", false).expect("sort_by");
    // After sort descending, first row id should be 3.
    let id_col = sorted
        .batches
        .first()
        .expect("one batch")
        .column_by_name("id")
        .expect("id col");
    let id_arr = id_col
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("Int64Array");
    assert_eq!(id_arr.value(0), 3);
}

// ── filter ────────────────────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_filter() {
    let table = make_table_with_data();
    // Keep only rows where id >= 2
    let pred = Predicate::Cmp {
        column: "id".to_string(),
        op: CmpOp::Ge,
        value: Scalar::Int64(2),
    };
    let filtered = ColumnarStore::filter(&table, &pred).expect("trait filter failed");
    assert_eq!(ColumnarStore::row_count(&filtered), 2);
}

// ── write_to_bytes ────────────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_write_bytes() {
    let table = make_table_with_data();
    let via_trait = ColumnarStore::write_to_bytes(&table).expect("trait write_to_bytes");
    let via_inherent = table.write_to_bytes().expect("inherent write_to_bytes");
    // Both should produce valid Parquet; lengths may differ by a few bytes due
    // to metadata timestamps — just assert both are non-empty and parseable.
    assert!(!via_trait.is_empty(), "trait bytes must be non-empty");
    assert!(!via_inherent.is_empty(), "inherent bytes must be non-empty");
    // Roundtrip the trait output.
    let reloaded = ColumnarTable::read_from_bytes(&via_trait).expect("roundtrip from trait bytes");
    assert_eq!(reloaded.row_count(), 3);
}

// ── write_to (file) ───────────────────────────────────────────────────────────

#[test]
fn columnar_store_trait_write_to_file() {
    let table = make_table_with_data();
    let tmp = std::env::temp_dir().join(format!(
        "columnar_store_trait_write_to_{}.parquet",
        std::process::id()
    ));
    ColumnarStore::write_to(&table, &tmp).expect("trait write_to failed");
    let reloaded = ColumnarTable::read_from(&tmp).expect("read_from failed");
    assert_eq!(reloaded.row_count(), 3);
    let _ = std::fs::remove_file(&tmp);
}

// ── write_to_bytes_with_config ────────────────────────────────────────────────

#[test]
fn columnar_store_trait_write_to_bytes_with_config() {
    let table = make_table_with_data();
    let config = WriterConfig {
        max_row_group_size: Some(1024),
    };
    let bytes = ColumnarStore::write_to_bytes_with_config(&table, &config)
        .expect("write_to_bytes_with_config");
    let reloaded = ColumnarTable::read_from_bytes(&bytes).expect("roundtrip");
    assert_eq!(reloaded.row_count(), 3);
}

// ── write_to_with_config (file) ───────────────────────────────────────────────

#[test]
fn columnar_store_trait_write_to_with_config() {
    let table = make_table_with_data();
    let config = WriterConfig {
        max_row_group_size: Some(512),
    };
    let tmp = std::env::temp_dir().join(format!(
        "columnar_store_trait_cfg_{}.parquet",
        std::process::id()
    ));
    ColumnarStore::write_to_with_config(&table, &tmp, &config)
        .expect("write_to_with_config failed");
    let reloaded = ColumnarTable::read_from(&tmp).expect("read_from failed");
    assert_eq!(reloaded.row_count(), 3);
    let _ = std::fs::remove_file(&tmp);
}

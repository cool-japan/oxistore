//! Tests for reading externally-generated Parquet files.
//!
//! These tests simulate Parquet files produced by external tools such as
//! pyarrow or pandas by generating Parquet bytes using the low-level
//! `parquet` crate directly — bypassing our `oxistore-columnar` writer
//! with its custom encoding/statistics policy.  The resulting payloads
//! have different encoding choices (e.g. PLAIN encoding instead of
//! DELTA_BINARY_PACKED) and may include features like row-group-level
//! metadata written differently.
//!
//! Assertions focus on:
//! - All data values survive the round-trip unchanged.
//! - Null values are preserved.
//! - Column projections still work on externally-written files.
//! - Schema evolution (null-fill) works on externally-written files.

use std::sync::Arc;

use arrow::array::{Array, Float64Array, Int32Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use oxistore_columnar::{read_batches_from_bytes, ColumnarTable};
use parquet::arrow::arrow_writer::ArrowWriter;
use parquet::basic::{Compression, Encoding};
use parquet::file::properties::WriterProperties;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Write `batches` to raw Parquet bytes using PLAIN encoding and no dictionary,
/// which is the default mode used by many external tools (pyarrow "PLAIN" tables,
/// pandas `to_parquet(engine='pyarrow', compression=None, use_dictionary=False)`).
fn write_plain_encoded(schema: Arc<Schema>, batches: &[RecordBatch]) -> Vec<u8> {
    let props = WriterProperties::builder()
        .set_compression(Compression::UNCOMPRESSED)
        .set_dictionary_enabled(false)
        .set_column_encoding(
            parquet::schema::types::ColumnPath::from("id"),
            Encoding::PLAIN,
        )
        .build();

    let mut buf = Vec::new();
    let mut writer =
        ArrowWriter::try_new(&mut buf, Arc::clone(&schema), Some(props)).expect("writer init");
    for batch in batches {
        writer.write(batch).expect("write batch");
    }
    writer.close().expect("close writer");
    buf
}

/// Write `batches` using default WriterProperties (no custom encoding settings),
/// simulating what a generic external tool would produce.
fn write_default_props(schema: Arc<Schema>, batches: &[RecordBatch]) -> Vec<u8> {
    let props = WriterProperties::builder()
        .set_compression(Compression::UNCOMPRESSED)
        .build();

    let mut buf = Vec::new();
    let mut writer =
        ArrowWriter::try_new(&mut buf, Arc::clone(&schema), Some(props)).expect("writer init");
    for batch in batches {
        writer.write(batch).expect("write batch");
    }
    writer.close().expect("close writer");
    buf
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Read an integer + string table written with PLAIN encoding (simulating pyarrow
/// default output when `use_dictionary=False` is requested).
#[test]
fn read_plain_encoded_int_string_table() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("label", DataType::Utf8, false),
    ]));

    let ids = Int64Array::from(vec![10i64, 20, 30, 40, 50]);
    let labels = StringArray::from(vec![
        Some("alpha"),
        Some("beta"),
        Some("gamma"),
        Some("delta"),
        Some("epsilon"),
    ]);
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(ids), Arc::new(labels)])
        .expect("batch");

    let bytes = write_plain_encoded(Arc::clone(&schema), &[batch]);

    // Read with our reader — must work regardless of encoding.
    let batches = read_batches_from_bytes(&bytes).expect("read plain encoded");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 5);

    let id_col = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("Int64Array");
    assert_eq!(id_col.value(0), 10);
    assert_eq!(id_col.value(4), 50);

    let label_col = batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("StringArray");
    assert_eq!(label_col.value(0), "alpha");
    assert_eq!(label_col.value(4), "epsilon");
}

/// Nullable columns from externally-generated files are preserved correctly.
#[test]
#[allow(clippy::approx_constant)]
fn read_external_nullable_columns() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("val", DataType::Float64, true),
        Field::new("name", DataType::Utf8, true),
    ]));

    // 3.14 is a test value representing a 2-decimal-place float, not PI.
    let test_val: f64 = 3.14;
    let vals = Float64Array::from(vec![Some(1.0), None, Some(test_val), None, Some(2.71)]);
    let names = StringArray::from(vec![Some("one"), Some("two"), None, Some("four"), None]);
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(vals), Arc::new(names)])
        .expect("batch");

    let bytes = write_default_props(Arc::clone(&schema), &[batch]);
    let batches = read_batches_from_bytes(&bytes).expect("read");

    assert_eq!(batches.len(), 1);
    let b = &batches[0];
    assert_eq!(b.num_rows(), 5);

    let val_col = b
        .column(0)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("f64");
    assert!(val_col.is_valid(0));
    assert!(val_col.is_null(1));
    assert!((val_col.value(2) - test_val).abs() < 1e-9);
    assert!(val_col.is_null(3));

    let name_col = b
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("str");
    assert!(name_col.is_valid(0));
    assert!(name_col.is_valid(1));
    assert!(name_col.is_null(2));
    assert!(name_col.is_null(4));
}

/// Multiple row groups from an externally-generated multi-batch file are
/// concatenated and readable in full.
#[test]
fn read_external_multi_row_group_file() {
    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int32, false)]));

    // Use a small row-group size to force multiple row groups, simulating
    // a pandas DataFrame.to_parquet() with row_group_size set.
    let props = WriterProperties::builder()
        .set_compression(Compression::UNCOMPRESSED)
        .set_max_row_group_row_count(Some(50))
        .build();

    let vals: Vec<i32> = (0i32..200).collect();
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int32Array::from(vals))])
        .expect("batch");

    let mut buf = Vec::new();
    let mut writer =
        ArrowWriter::try_new(&mut buf, Arc::clone(&schema), Some(props)).expect("writer");
    writer.write(&batch).expect("write");
    writer.close().expect("close");

    // Verify metadata shows multiple row groups.
    let meta = oxistore_columnar::read_metadata_from_bytes(&buf).expect("metadata");
    assert!(
        meta.num_row_groups >= 4,
        "expected at least 4 row groups, got {}",
        meta.num_row_groups
    );
    assert_eq!(meta.num_rows, 200);

    // Read back all data.
    let batches = read_batches_from_bytes(&buf).expect("read");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total, 200);
}

/// Column projection works on externally-generated files (column pruning).
#[test]
fn read_external_with_column_projection() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("score", DataType::Float64, false),
        Field::new("active", DataType::Int32, false),
    ]));

    let ids = Int64Array::from(vec![1i64, 2, 3, 4, 5]);
    let names = StringArray::from(vec![Some("a"), Some("b"), Some("c"), Some("d"), Some("e")]);
    let scores = Float64Array::from(vec![1.1, 2.2, 3.3, 4.4, 5.5]);
    let active = Int32Array::from(vec![1i32, 0, 1, 1, 0]);
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(ids),
            Arc::new(names),
            Arc::new(scores),
            Arc::new(active),
        ],
    )
    .expect("batch");

    let bytes = write_default_props(Arc::clone(&schema), &[batch]);

    // Read only 2 of the 4 columns.
    let table = ColumnarTable::read_columns(&bytes, &["id", "score"]).expect("read columns");
    assert_eq!(table.row_count(), 5);
    assert_eq!(table.schema.fields().len(), 2);
    assert_eq!(table.schema.field(0).name(), "id");
    assert_eq!(table.schema.field(1).name(), "score");
}

/// Schema evolution (null-fill) works on externally-generated files with fewer
/// columns than the target schema expects.
#[test]
fn read_external_with_schema_evolution_null_fill() {
    // External file has only 2 columns.
    let schema_external = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("value", DataType::Float64, true),
    ]));

    let ids = Int64Array::from(vec![100i64, 200, 300]);
    let vals = Float64Array::from(vec![Some(1.5), Some(2.5), Some(3.5)]);
    let batch = RecordBatch::try_new(
        Arc::clone(&schema_external),
        vec![Arc::new(ids), Arc::new(vals)],
    )
    .expect("batch");

    let bytes = write_default_props(Arc::clone(&schema_external), &[batch]);

    // Read with a target schema that has an extra column.
    let target_schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("value", DataType::Float64, true),
        Field::new("tag", DataType::Utf8, true), // not in the external file
    ]));

    let table = ColumnarTable::read_with_schema(&bytes, &target_schema).expect("schema evolution");
    assert_eq!(table.row_count(), 3);
    assert_eq!(table.schema.fields().len(), 3);

    // The 'tag' column must exist and be all-null.
    let tag_col = table.batches[0]
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("StringArray");
    for i in 0..3 {
        assert!(Array::is_null(tag_col, i), "tag[{i}] should be null");
    }
}

/// Predicate pushdown works on externally-generated files (row group pruning
/// via statistics that external writers also write by default).
#[test]
fn read_external_with_predicate_pushdown() {
    use oxistore_columnar::predicate::{CmpOp, Predicate, Scalar};

    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));

    // Write 3 row groups: [0..99], [100..199], [200..299].
    let props = WriterProperties::builder()
        .set_compression(Compression::UNCOMPRESSED)
        .set_max_row_group_row_count(Some(100))
        .build();

    let vals: Vec<i64> = (0i64..300).collect();
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
        .expect("batch");

    let mut buf = Vec::new();
    let mut writer =
        ArrowWriter::try_new(&mut buf, Arc::clone(&schema), Some(props)).expect("writer");
    writer.write(&batch).expect("write");
    writer.close().expect("close");

    // Row group layout (100 rows each):
    //   Group 0: x=0..99   (max=99)
    //   Group 1: x=100..199 (max=199)
    //   Group 2: x=200..299 (max=299)
    //
    // Predicate x > 150 at the row-group level:
    //   Group 0: max=99, 99 > 150? No  → pruned
    //   Group 1: max=199, 199 > 150? Yes → kept (contains values 100..199)
    //   Group 2: max=299, 299 > 150? Yes → kept (contains values 200..299)
    //
    // Note: `read_with_predicate` performs row-GROUP pruning only.  It skips
    // groups that provably cannot match, but rows within surviving groups are
    // returned as-is.  Row-level filtering must be applied separately if needed.
    let pred = Predicate::Cmp {
        column: "x".to_string(),
        op: CmpOp::Gt,
        value: Scalar::Int64(150),
    };
    let table = ColumnarTable::read_with_predicate(&buf, &pred).expect("predicate read");

    // Row group 0 (x in [0..99]) must have been pruned: all returned rows
    // must have x >= 100 (from groups 1 and 2).
    for b in &table.batches {
        let col = b
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("i64");
        for i in 0..col.len() {
            assert!(
                col.value(i) >= 100,
                "value {} belongs to pruned row group 0 (x < 100)",
                col.value(i)
            );
        }
    }

    // Groups 1 and 2 survive → total row count should be 200 (not 300).
    // Group 0 (100 rows) was pruned.
    assert_eq!(
        table.row_count(),
        200,
        "expected 200 rows (groups 1 and 2), got {}",
        table.row_count()
    );
}

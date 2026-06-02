/// Comprehensive test suite for oxistore-columnar.
///
/// Covers: multi-type round trips, large tables, row-group boundaries,
/// empty tables, column projection, predicate filtering, metadata extraction,
/// and multi-batch accumulation.
use std::sync::Arc;

use oxistore_columnar::{
    Array, CmpOp, ColumnarTable, ColumnarTableBuilder, DataType, Field, Float64Array, Int32Array,
    Int64Array, Predicate, RecordBatch, Scalar, Schema, StringArray, WriterConfig,
};

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn pid_suffix() -> u32 {
    std::process::id()
}

// ---------------------------------------------------------------------------
// 1. All supported types round-trip
// ---------------------------------------------------------------------------

/// Write a RecordBatch containing Int32, Int64, Float64, Utf8, and Boolean
/// columns. Read back and verify schema and values are preserved.
#[test]
fn columnar_all_supported_types_round_trip() {
    use arrow::array::BooleanArray;

    let schema = Arc::new(Schema::new(vec![
        Field::new("col_i32", DataType::Int32, false),
        Field::new("col_i64", DataType::Int64, false),
        Field::new("col_f64", DataType::Float64, false),
        Field::new("col_str", DataType::Utf8, true),
        Field::new("col_bool", DataType::Boolean, false),
    ]));

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int32Array::from(vec![1i32, 2, 3])),
            Arc::new(Int64Array::from(vec![10i64, 20, 30])),
            Arc::new(Float64Array::from(vec![1.1f64, 2.2, 3.3])),
            Arc::new(StringArray::from(vec![Some("alpha"), None, Some("gamma")])),
            Arc::new(BooleanArray::from(vec![true, false, true])),
        ],
    )
    .expect("batch construction");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let bytes = table.write_to_bytes().expect("write_to_bytes");
    let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

    // Schema preserved.
    assert_eq!(loaded.schema.fields().len(), 5, "schema field count");
    assert_eq!(loaded.schema.field(0).name(), "col_i32");
    assert_eq!(loaded.schema.field(0).data_type(), &DataType::Int32);
    assert_eq!(loaded.schema.field(1).name(), "col_i64");
    assert_eq!(loaded.schema.field(1).data_type(), &DataType::Int64);
    assert_eq!(loaded.schema.field(2).name(), "col_f64");
    assert_eq!(loaded.schema.field(2).data_type(), &DataType::Float64);
    assert_eq!(loaded.schema.field(3).name(), "col_str");
    assert_eq!(loaded.schema.field(3).data_type(), &DataType::Utf8);
    assert_eq!(loaded.schema.field(4).name(), "col_bool");
    assert_eq!(loaded.schema.field(4).data_type(), &DataType::Boolean);

    // Row count preserved.
    assert_eq!(loaded.row_count(), 3, "row count");

    // Int32 values.
    let i32_col = loaded.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("downcast Int32Array");
    assert_eq!(i32_col.value(0), 1);
    assert_eq!(i32_col.value(1), 2);
    assert_eq!(i32_col.value(2), 3);

    // Int64 values.
    let i64_col = loaded.batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("downcast Int64Array");
    assert_eq!(i64_col.value(0), 10);
    assert_eq!(i64_col.value(1), 20);
    assert_eq!(i64_col.value(2), 30);

    // Float64 values.
    let f64_col = loaded.batches[0]
        .column(2)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("downcast Float64Array");
    assert!((f64_col.value(0) - 1.1f64).abs() < 1e-9);
    assert!((f64_col.value(1) - 2.2f64).abs() < 1e-9);

    // Utf8 — null at index 1.
    let str_col = loaded.batches[0]
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("downcast StringArray");
    assert_eq!(str_col.value(0), "alpha");
    assert!(str_col.is_null(1), "index 1 must be null");
    assert_eq!(str_col.value(2), "gamma");

    // Boolean values.
    let bool_col = loaded.batches[0]
        .column(4)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("downcast BooleanArray");
    assert!(bool_col.value(0));
    assert!(!bool_col.value(1));
    assert!(bool_col.value(2));
}

// ---------------------------------------------------------------------------
// 2. Large table (10k rows)
// ---------------------------------------------------------------------------

/// Write a single Int64 batch with 10 000 rows, read back and verify count.
#[test]
fn columnar_large_table_10k_rows() {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));

    let vals: Vec<i64> = (0i64..10_000).collect();
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
        .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let path = std::env::temp_dir().join(format!("oxistore_col_large_{}.parquet", pid_suffix()));
    table.write_to(&path).expect("write_to");

    let loaded = ColumnarTable::read_from(&path).expect("read_from");
    let _ = std::fs::remove_file(&path);

    assert_eq!(loaded.row_count(), 10_000, "expected 10 000 rows");

    // Spot-check first and last value.
    let first_batch = &loaded.batches[0];
    let col = first_batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("Int64Array");
    assert_eq!(col.value(0), 0i64);
}

// ---------------------------------------------------------------------------
// 3. Row-group boundary
// ---------------------------------------------------------------------------

/// Push 250 rows with max_row_group_size=100; the file should contain >= 2
/// row groups (parquet will create 3: 100 + 100 + 50).
#[test]
fn columnar_row_group_boundary() {
    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));

    let (mut table, config) = ColumnarTableBuilder::new(Arc::clone(&schema))
        .row_group_size(100)
        .build_with_config();

    let vals: Vec<i64> = (0i64..250).collect();
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
        .expect("batch");
    table.push(batch).expect("push");

    let bytes = table.write_to_bytes_with_config(&config).expect("write");
    let meta = oxistore_columnar::read_metadata_from_bytes(&bytes).expect("metadata");

    assert!(
        meta.num_row_groups >= 2,
        "expected >= 2 row groups with size=100 and 250 rows, got {}",
        meta.num_row_groups
    );
    assert_eq!(meta.num_rows, 250, "total row count must be 250");
}

// ---------------------------------------------------------------------------
// 4. Empty table (zero rows)
// ---------------------------------------------------------------------------

/// Write an empty table (no push calls) and verify metadata reports 0 rows.
#[test]
fn columnar_empty_table_zero_rows() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("label", DataType::Utf8, true),
    ]));

    let table = ColumnarTable::new(Arc::clone(&schema));
    assert_eq!(table.row_count(), 0, "table must start empty");

    let bytes = table.write_to_bytes().expect("write_to_bytes");
    assert!(
        !bytes.is_empty(),
        "even an empty parquet file is non-empty bytes"
    );

    let meta = oxistore_columnar::read_metadata_from_bytes(&bytes).expect("metadata");
    assert_eq!(meta.num_rows, 0, "metadata must report 0 rows");
    assert_eq!(meta.num_columns, 2, "schema has 2 columns");
}

// ---------------------------------------------------------------------------
// 5. Column projection (two of three columns)
// ---------------------------------------------------------------------------

/// Write a table with 3 columns (a, b, c). Read back selecting only a and c.
/// Verify output schema has 2 columns and row count is correct.
#[test]
fn columnar_projection_two_of_three_columns() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("a", DataType::Int32, false),
        Field::new("b", DataType::Int64, false),
        Field::new("c", DataType::Utf8, true),
    ]));

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int32Array::from((0i32..10).collect::<Vec<_>>())),
            Arc::new(Int64Array::from((0i64..10).collect::<Vec<_>>())),
            Arc::new(StringArray::from(
                (0..10).map(|i| format!("v{i}")).collect::<Vec<_>>(),
            )),
        ],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");
    let bytes = table.write_to_bytes().expect("write_to_bytes");

    let projected = ColumnarTable::read_columns(&bytes, &["a", "c"]).expect("read_columns");

    assert_eq!(projected.schema.fields().len(), 2, "expected 2 columns");
    assert_eq!(projected.schema.field(0).name(), "a");
    assert_eq!(projected.schema.field(1).name(), "c");
    assert_eq!(projected.row_count(), 10, "all 10 rows");
}

// ---------------------------------------------------------------------------
// 6. Predicate filter (value > 50)
// ---------------------------------------------------------------------------

/// Write an Int32 column with values 0..100. Filter with > 50 at row level.
/// All returned rows must satisfy value > 50 (expect 49 rows: 51..=99).
#[test]
fn columnar_predicate_filter_gt50() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int32Array::from((0i32..100).collect::<Vec<_>>()))],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let pred = Predicate::Cmp {
        column: "value".to_string(),
        op: CmpOp::Gt,
        value: Scalar::Int32(50),
    };

    let result = table.filter(&pred).expect("filter");
    assert_eq!(
        result.row_count(),
        49,
        "values 51..=99 → 49 rows, got {}",
        result.row_count()
    );

    // Verify every surviving value is > 50.
    for batch in &result.batches {
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .expect("Int32Array");
        for i in 0..col.len() {
            assert!(
                col.value(i) > 50,
                "row {} has value {} <= 50",
                i,
                col.value(i)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 7. Metadata extraction
// ---------------------------------------------------------------------------

/// Write a table with 5 rows and 2 columns. Verify metadata_from_bytes.
#[test]
fn columnar_metadata_extraction() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("x", DataType::Int32, false),
        Field::new("y", DataType::Float64, false),
    ]));

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int32Array::from(vec![1i32, 2, 3, 4, 5])),
            Arc::new(Float64Array::from(vec![0.1f64, 0.2, 0.3, 0.4, 0.5])),
        ],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");
    let bytes = table.write_to_bytes().expect("write_to_bytes");

    let meta = ColumnarTable::metadata_from_bytes(&bytes).expect("metadata_from_bytes");
    assert_eq!(meta.num_rows, 5, "expected 5 rows");
    assert_eq!(meta.num_columns, 2, "expected 2 columns");
    assert!(meta.num_row_groups >= 1, "at least one row group");
    assert_eq!(
        meta.file_size,
        bytes.len() as u64,
        "file_size == bytes.len()"
    );
}

// ---------------------------------------------------------------------------
// 8. Multiple batches accumulated
// ---------------------------------------------------------------------------

/// Push 3 separate batches of 100 rows each. Read back. Verify total == 300.
#[test]
fn columnar_multiple_batches_accumulated() {
    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));

    let make_batch = |offset: i64| {
        let vals: Vec<i64> = (offset..offset + 100).collect();
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
            .expect("batch")
    };

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(make_batch(0)).expect("push batch 0");
    table.push(make_batch(100)).expect("push batch 1");
    table.push(make_batch(200)).expect("push batch 2");

    assert_eq!(table.row_count(), 300, "in-memory row count before write");

    let path =
        std::env::temp_dir().join(format!("oxistore_col_multi_batch_{}.parquet", pid_suffix()));
    table.write_to(&path).expect("write_to");

    let loaded = ColumnarTable::read_from(&path).expect("read_from");
    let _ = std::fs::remove_file(&path);

    assert_eq!(loaded.row_count(), 300, "loaded row count must be 300");
}

// ---------------------------------------------------------------------------
// 9. ColumnarTableBuilder row_group_size_hint propagation
// ---------------------------------------------------------------------------

/// Builder's row_group_size_hint() must return the value set via row_group_size().
#[test]
fn columnar_builder_row_group_hint_propagation() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let builder = ColumnarTableBuilder::new(Arc::clone(&schema)).row_group_size(256);

    assert_eq!(builder.row_group_size_hint(), Some(256));

    let (table, config) = builder.build_with_config();
    assert_eq!(table.row_count(), 0, "freshly built table is empty");
    assert_eq!(config.max_row_group_size, Some(256));
}

// ---------------------------------------------------------------------------
// 10. write_to_with_config + read_metadata (file-based)
// ---------------------------------------------------------------------------

/// Write with a custom WriterConfig via write_to_with_config, then read
/// file-level metadata and verify row_group count.
#[test]
fn columnar_write_to_with_config_file() {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int32, false)]));

    let vals: Vec<i32> = (0i32..300).collect();
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int32Array::from(vals))])
        .expect("batch");

    let config = WriterConfig {
        max_row_group_size: Some(100),
    };

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let path = std::env::temp_dir().join(format!("oxistore_col_config_{}.parquet", pid_suffix()));
    table
        .write_to_with_config(&path, &config)
        .expect("write_to_with_config");

    let meta = oxistore_columnar::read_metadata(&path).expect("read_metadata");
    let _ = std::fs::remove_file(&path);

    assert_eq!(meta.num_rows, 300, "total rows");
    assert!(
        meta.num_row_groups >= 3,
        "expected >= 3 row groups, got {}",
        meta.num_row_groups
    );
    assert_eq!(meta.num_columns, 1);
    assert!(meta.file_size > 0);
}

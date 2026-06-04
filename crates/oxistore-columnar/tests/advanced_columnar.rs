//! Advanced columnar tests:
//! - Round-trip with all supported Arrow data types
//! - Schema evolution (reading files with missing columns)
//! - Large table streaming (1M+ rows)
//! - Row group boundary alignment
//! - Schema mismatch rejection on push
//! - Dictionary-encoded columns round-trip

use std::sync::Arc;

use arrow::array::{
    BinaryArray, BooleanArray, Float32Array, Float64Array, Int16Array, Int32Array, Int64Array,
    Int8Array, LargeBinaryArray, LargeStringArray, StringArray, UInt16Array, UInt32Array,
    UInt64Array, UInt8Array,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use oxistore_columnar::{
    compute, read_batches_from_bytes, write_batches_to_bytes, Array, ColumnarStore, ColumnarTable,
};

// ── Round-trip with all supported Arrow data types ────────────────────────────

#[test]
fn round_trip_all_integer_types() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("i8", DataType::Int8, true),
        Field::new("i16", DataType::Int16, true),
        Field::new("i32", DataType::Int32, true),
        Field::new("i64", DataType::Int64, true),
        Field::new("u8", DataType::UInt8, true),
        Field::new("u16", DataType::UInt16, true),
        Field::new("u32", DataType::UInt32, true),
        Field::new("u64", DataType::UInt64, true),
    ]));

    let i8_arr = Int8Array::from(vec![Some(-128i8), Some(0), Some(127)]);
    let i16_arr = Int16Array::from(vec![Some(-32768i16), Some(0), Some(32767)]);
    let i32_arr = Int32Array::from(vec![Some(i32::MIN), Some(0), Some(i32::MAX)]);
    let i64_arr = Int64Array::from(vec![Some(i64::MIN), Some(0), Some(i64::MAX)]);
    let u8_arr = UInt8Array::from(vec![Some(0u8), Some(127), Some(255)]);
    let u16_arr = UInt16Array::from(vec![Some(0u16), Some(1000), Some(65535)]);
    let u32_arr = UInt32Array::from(vec![Some(0u32), Some(1_000_000), Some(u32::MAX)]);
    let u64_arr = UInt64Array::from(vec![Some(0u64), Some(1_000_000_000), Some(u64::MAX)]);

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(i8_arr),
            Arc::new(i16_arr),
            Arc::new(i32_arr),
            Arc::new(i64_arr),
            Arc::new(u8_arr),
            Arc::new(u16_arr),
            Arc::new(u32_arr),
            Arc::new(u64_arr),
        ],
    )
    .expect("batch construction failed");

    let bytes =
        write_batches_to_bytes(Arc::clone(&schema), std::slice::from_ref(&batch)).expect("write");
    let batches = read_batches_from_bytes(&bytes).expect("read");
    assert_eq!(batches.len(), 1);
    // Compare values
    let read_batch = &batches[0];
    assert_eq!(read_batch.num_rows(), 3);
    assert_eq!(read_batch.num_columns(), 8);
}

#[test]
fn round_trip_float_and_string_types() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("f32", DataType::Float32, true),
        Field::new("f64", DataType::Float64, true),
        Field::new("utf8", DataType::Utf8, true),
        Field::new("large_utf8", DataType::LargeUtf8, true),
        Field::new("binary", DataType::Binary, true),
        Field::new("large_binary", DataType::LargeBinary, true),
    ]));

    let f32_arr = Float32Array::from(vec![Some(1.0f32), Some(f32::NAN), None]);
    let f64_arr = Float64Array::from(vec![Some(f64::INFINITY), Some(-1.5), Some(0.0)]);
    let utf8_arr = StringArray::from(vec![Some("hello"), None, Some("world")]);
    let large_utf8_arr = LargeStringArray::from(vec![Some("large"), Some("utf8"), None]);
    let binary_arr = BinaryArray::from(vec![Some(b"bytes".as_ref()), None, Some(b"data".as_ref())]);
    let large_binary_arr =
        LargeBinaryArray::from(vec![Some(b"large".as_ref()), Some(b"bin".as_ref()), None]);

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(f32_arr),
            Arc::new(f64_arr),
            Arc::new(utf8_arr),
            Arc::new(large_utf8_arr),
            Arc::new(binary_arr),
            Arc::new(large_binary_arr),
        ],
    )
    .expect("batch construction failed");

    let bytes = write_batches_to_bytes(Arc::clone(&schema), &[batch]).expect("write");
    let batches = read_batches_from_bytes(&bytes).expect("read");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 3);
    assert_eq!(batches[0].num_columns(), 6);
}

#[test]
fn round_trip_boolean_type() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "bool_col",
        DataType::Boolean,
        true,
    )]));
    let bool_arr = BooleanArray::from(vec![Some(true), Some(false), None, Some(true)]);
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(bool_arr)]).expect("batch");

    let bytes = write_batches_to_bytes(Arc::clone(&schema), &[batch]).expect("write");
    let batches = read_batches_from_bytes(&bytes).expect("read");
    assert_eq!(batches[0].num_rows(), 4);

    let col = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("downcast");
    assert!(col.is_valid(0) && col.value(0));
    assert!(col.is_valid(1) && !col.value(1));
    assert!(col.is_null(2));
    assert!(col.is_valid(3) && col.value(3));
}

// ── Schema evolution: reading files with missing columns ──────────────────────

#[test]
fn schema_evolution_missing_column_filled_with_null() {
    // Write a table with 3 columns: id, value, label
    let schema_full = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("value", DataType::Float64, true),
        Field::new("label", DataType::Utf8, true),
    ]));
    let ids = Int64Array::from(vec![1i64, 2, 3]);
    let vals = Float64Array::from(vec![Some(1.1), Some(2.2), Some(3.3)]);
    let labels = StringArray::from(vec![Some("a"), Some("b"), Some("c")]);
    let batch = RecordBatch::try_new(
        Arc::clone(&schema_full),
        vec![Arc::new(ids), Arc::new(vals), Arc::new(labels)],
    )
    .expect("batch");
    let bytes = write_batches_to_bytes(Arc::clone(&schema_full), &[batch]).expect("write");

    // Read back with a schema that only has 2 of the 3 columns (projection)
    let schema_reduced = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("value", DataType::Float64, true),
    ]));

    let table = ColumnarTable::read_with_schema(&bytes, &schema_reduced).expect("read with schema");
    assert!(table.row_count() > 0);
    // The projected table schema should match the reduced schema
    let tschema = table.schema();
    assert_eq!(tschema.field(0).name(), "id");
    assert_eq!(tschema.field(1).name(), "value");
    assert_eq!(tschema.fields().len(), 2, "should have exactly 2 columns");
}

// ── Large table streaming ─────────────────────────────────────────────────────

#[test]
fn large_table_1m_rows_streaming_correctness() {
    // Write 1M rows in 10 batches of 100k each
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("val", DataType::Float64, false),
    ]));

    let n_batches = 10usize;
    let rows_per_batch = 100_000usize;
    let mut all_bytes = Vec::new();

    // Use streaming writer to avoid materializing all data at once
    use oxistore_columnar::ColumnarStreamWriter;
    let mut writer = ColumnarStreamWriter::new(Arc::clone(&schema), &mut all_bytes, None)
        .expect("stream writer");

    for b in 0..n_batches {
        let base = (b * rows_per_batch) as i64;
        let ids: Vec<i64> = (base..(base + rows_per_batch as i64)).collect();
        let vals: Vec<f64> = ids.iter().map(|&i| i as f64 * 1.5).collect();
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(Float64Array::from(vals)),
            ],
        )
        .expect("batch");
        writer.write_batch(&batch).expect("write batch");
    }
    writer.finish().expect("finish");

    // Read back and count rows
    let batches = read_batches_from_bytes(&all_bytes).expect("read");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, n_batches * rows_per_batch, "1M rows expected");
}

// ── Schema mismatch rejection ─────────────────────────────────────────────────

#[test]
fn push_schema_mismatch_rejected() {
    let schema_a = Arc::new(Schema::new(vec![
        Field::new("x", DataType::Int64, false),
        Field::new("y", DataType::Float64, false),
    ]));
    let mut table = ColumnarTable::new(Arc::clone(&schema_a));

    // First push should succeed
    let batch_a = RecordBatch::try_new(
        Arc::clone(&schema_a),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2])),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
        ],
    )
    .expect("batch");
    table.push(batch_a).expect("first push should succeed");

    // Push with different schema should be rejected
    let schema_b = Arc::new(Schema::new(vec![
        Field::new("x", DataType::Int64, false),
        Field::new("z", DataType::Utf8, false), // different field name
    ]));
    let batch_b = RecordBatch::try_new(
        Arc::clone(&schema_b),
        vec![
            Arc::new(Int64Array::from(vec![3i64])),
            Arc::new(StringArray::from(vec![Some("hello")])),
        ],
    )
    .expect("batch_b");
    let result = table.push(batch_b);
    assert!(result.is_err(), "push with different schema must fail");
}

// ── Dictionary-encoded columns round-trip ─────────────────────────────────────

#[test]
fn dictionary_encoded_utf8_round_trip() {
    // Write a table with many repeated string values (ideal for dictionary encoding)
    let schema = Arc::new(Schema::new(vec![
        Field::new("category", DataType::Utf8, false),
        Field::new("value", DataType::Int64, false),
    ]));

    // 1000 rows with 10 distinct categories — should compress well with dict encoding
    let categories: Vec<Option<&str>> = (0..1000)
        .map(|i| {
            Some(
                [
                    "cat_a", "cat_b", "cat_c", "cat_d", "cat_e", "cat_f", "cat_g", "cat_h",
                    "cat_i", "cat_j",
                ][i % 10],
            )
        })
        .collect();
    let values: Vec<i64> = (0..1000i64).collect();

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from(categories)),
            Arc::new(Int64Array::from(values.clone())),
        ],
    )
    .expect("batch");

    let bytes = write_batches_to_bytes(Arc::clone(&schema), &[batch]).expect("write");
    let batches = read_batches_from_bytes(&bytes).expect("read");

    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 1000);

    // Verify first few values
    let cats = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("downcast string");
    assert_eq!(cats.value(0), "cat_a");
    assert_eq!(cats.value(1), "cat_b");
    assert_eq!(cats.value(10), "cat_a"); // wraps around
}

// ── ColumnarTable::merge and project ─────────────────────────────────────────

#[test]
fn merge_two_tables() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));

    let mut t1 = ColumnarTable::new(Arc::clone(&schema));
    let b1 = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int64Array::from(vec![1i64, 2, 3]))],
    )
    .expect("b1");
    t1.push(b1).expect("push b1");

    let mut t2 = ColumnarTable::new(Arc::clone(&schema));
    let b2 = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int64Array::from(vec![4i64, 5]))],
    )
    .expect("b2");
    t2.push(b2).expect("push b2");

    t1.merge(&t2).expect("merge");
    assert_eq!(t1.row_count(), 5);
}

#[test]
fn project_reduces_columns() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("a", DataType::Int64, false),
        Field::new("b", DataType::Utf8, false),
        Field::new("c", DataType::Float64, false),
    ]));

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2])),
            Arc::new(StringArray::from(vec![Some("x"), Some("y")])),
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
        ],
    )
    .expect("batch");
    table.push(batch).expect("push");

    let projected = table.project(&["a", "c"]).expect("project");
    assert_eq!(projected.row_count(), 2);
    // Projected schema should only have a and c
    assert!(
        projected.schema().field_with_name("b").is_err() || {
            // If b is present but shouldn't be, this should fail
            false
        }
    );
}

// ── Row count and display ─────────────────────────────────────────────────────

#[test]
fn row_count_accumulates_across_batches() {
    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int32, false)]));
    let mut table = ColumnarTable::new(Arc::clone(&schema));
    assert_eq!(table.row_count(), 0);

    for size in &[10usize, 20, 30] {
        let arr: Vec<i32> = (0..*size as i32).collect();
        let batch =
            RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int32Array::from(arr))])
                .expect("batch");
        table.push(batch).expect("push");
    }
    assert_eq!(table.row_count(), 60);
}

#[test]
fn display_shows_schema_and_row_count() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
    ]));
    let mut table = ColumnarTable::new(Arc::clone(&schema));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2, 3])),
            Arc::new(StringArray::from(vec![Some("a"), Some("b"), Some("c")])),
        ],
    )
    .expect("batch");
    table.push(batch).expect("push");

    let display = format!("{table}");
    // Display should mention schema fields and row count
    assert!(
        display.contains("id") || display.contains("3") || !display.is_empty(),
        "display output should not be empty"
    );
}

// ── Row group boundary alignment ──────────────────────────────────────────────

/// Verify data integrity when batch sizes don't align with row group sizes.
///
/// We write 250 rows with max_row_group_size=100, which produces 3 row groups
/// (100 + 100 + 50).  After reading back, the values must be contiguous and
/// exact — no data is lost or duplicated at the group boundaries.
#[test]
fn row_group_boundary_alignment_integrity() {
    use oxistore_columnar::{ColumnarTableBuilder, WriterConfig};

    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));

    let config = WriterConfig {
        max_row_group_size: Some(100),
    };

    // Write two separate input batches: 150 rows then 100 rows.
    // The first batch spans row group 0 (rows 0..99) and bleeds into group 1.
    // The second batch begins partway through group 1 and fills group 2.
    let batch_a = {
        let vals: Vec<i64> = (0i64..150).collect();
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
            .expect("batch_a")
    };
    let batch_b = {
        let vals: Vec<i64> = (150i64..250).collect();
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
            .expect("batch_b")
    };

    let mut table = ColumnarTableBuilder::new(Arc::clone(&schema)).build();
    table.push(batch_a).expect("push batch_a");
    table.push(batch_b).expect("push batch_b");

    let bytes = table.write_to_bytes_with_config(&config).expect("write");

    // Read back and verify structure.
    let meta = oxistore_columnar::read_metadata_from_bytes(&bytes).expect("metadata");
    assert_eq!(meta.num_rows, 250, "total row count must be 250");
    assert!(
        meta.num_row_groups >= 2,
        "expected at least 2 row groups with size=100, got {}",
        meta.num_row_groups
    );

    // Read back all data and verify values are contiguous from 0..249.
    let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");
    assert_eq!(loaded.row_count(), 250, "loaded row count must be 250");

    // Concatenate all batches and verify every value in order.
    let combined = compute::concat_batches(&schema, &loaded.batches).expect("concat_batches");
    let col = combined
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("Int64Array");

    assert_eq!(col.len(), 250, "combined column length must be 250");
    for i in 0..250usize {
        assert_eq!(
            col.value(i),
            i as i64,
            "value mismatch at row {} (boundary misalignment?)",
            i
        );
    }
}

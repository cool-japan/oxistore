use std::sync::Arc;

use std::path::PathBuf;

use oxistore_columnar::{
    CmpOp, ColumnarStreamReader, ColumnarStreamWriter, ColumnarTable, ColumnarTableBuilder,
    DataType, Field, Float32Array, Float64Array, Int32Array, Int64Array, PartitionPredicate,
    PartitionedDataset, Predicate, RecordBatch, Scalar, Schema, StringArray, WriterConfig,
};

fn make_test_table() -> ColumnarTable {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
    ]));
    let mut table = ColumnarTable::new(Arc::clone(&schema));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![3, 1, 2])),
            Arc::new(StringArray::from(vec!["charlie", "alice", "bob"])),
        ],
    )
    .expect("batch");
    table.push_unchecked(batch);
    table
}

#[test]
fn row_count() {
    let table = make_test_table();
    assert_eq!(table.row_count(), 3);
}

#[test]
fn row_count_empty() {
    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
    let table = ColumnarTable::new(schema);
    assert_eq!(table.row_count(), 0);
}

#[test]
fn project_single_column() {
    let table = make_test_table();
    let projected = table.project(&["name"]).expect("project");
    assert_eq!(projected.schema.fields().len(), 1);
    assert_eq!(projected.schema.field(0).name(), "name");
    assert_eq!(projected.row_count(), 3);
}

#[test]
fn project_missing_column_ignored() {
    let table = make_test_table();
    let projected = table.project(&["id", "nonexistent"]).expect("project");
    assert_eq!(projected.schema.fields().len(), 1);
    assert_eq!(projected.schema.field(0).name(), "id");
}

#[test]
fn sort_by_ascending() {
    let table = make_test_table();
    let sorted = table.sort_by("id", true).expect("sort");
    assert_eq!(sorted.row_count(), 3);
    assert_eq!(sorted.batches.len(), 1);

    let ids = sorted.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("int64");
    assert_eq!(ids.value(0), 1);
    assert_eq!(ids.value(1), 2);
    assert_eq!(ids.value(2), 3);
}

#[test]
fn sort_by_descending() {
    let table = make_test_table();
    let sorted = table.sort_by("id", false).expect("sort");
    let ids = sorted.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("int64");
    assert_eq!(ids.value(0), 3);
    assert_eq!(ids.value(1), 2);
    assert_eq!(ids.value(2), 1);
}

#[test]
fn merge_tables() {
    let table1 = make_test_table();
    let mut table2 = make_test_table();
    table2.merge(&table1).expect("merge");
    assert_eq!(table2.row_count(), 6);
    assert_eq!(table2.batches.len(), 2);
}

#[test]
fn merge_schema_mismatch_fails() {
    let table1 = make_test_table();
    let schema2 = Arc::new(Schema::new(vec![Field::new("x", DataType::Float64, false)]));
    let mut table2 = ColumnarTable::new(schema2);
    assert!(table2.merge(&table1).is_err());
}

#[test]
fn push_schema_validation() {
    let table_schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let mut table = ColumnarTable::new(Arc::clone(&table_schema));

    // Correct schema succeeds.
    let good_batch = RecordBatch::try_new(
        Arc::clone(&table_schema),
        vec![Arc::new(Int64Array::from(vec![1]))],
    )
    .expect("batch");
    assert!(table.push(good_batch).is_ok());

    // Wrong schema fails.
    let bad_schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Utf8, false)]));
    let bad_batch =
        RecordBatch::try_new(bad_schema, vec![Arc::new(StringArray::from(vec!["oops"]))])
            .expect("batch");
    assert!(table.push(bad_batch).is_err());
}

#[test]
fn write_to_and_read_from_bytes() {
    let table = make_test_table();
    let bytes = table.write_to_bytes().expect("write_to_bytes");
    assert!(!bytes.is_empty());

    let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");
    assert_eq!(loaded.row_count(), 3);
    assert_eq!(loaded.schema.fields().len(), 2);
}

#[test]
fn read_metadata() {
    let path = std::env::temp_dir().join(format!(
        "oxistore-columnar-meta-{}.parquet",
        std::process::id()
    ));
    let table = make_test_table();
    table.write_to(&path).expect("write");

    let meta = oxistore_columnar::read_metadata(&path).expect("metadata");
    assert_eq!(meta.num_rows, 3);
    assert_eq!(meta.num_columns, 2);
    assert!(meta.file_size > 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn read_with_projection() {
    let path = std::env::temp_dir().join(format!(
        "oxistore-columnar-proj-{}.parquet",
        std::process::id()
    ));
    let table = make_test_table();
    table.write_to(&path).expect("write");

    // Read only column 0 (id).
    let batches = oxistore_columnar::read_batches_with_projection(&path, &[0]).expect("read");
    assert!(!batches.is_empty());
    assert_eq!(batches[0].num_columns(), 1);
    assert_eq!(batches[0].num_rows(), 3);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn display_impl() {
    let table = make_test_table();
    let s = format!("{table}");
    assert!(s.contains("2 cols"));
    assert!(s.contains("3 rows"));
}

// ---------------------------------------------------------------------------
// Projection tests (read_columns)
// ---------------------------------------------------------------------------

/// Write a 5-column table to bytes, read back with only 2 columns projected.
#[test]
fn projection_read_columns_two_of_five() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("a", DataType::Int64, false),
        Field::new("b", DataType::Int32, false),
        Field::new("c", DataType::Float64, false),
        Field::new("d", DataType::Float32, false),
        Field::new("e", DataType::Utf8, false),
    ]));

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2, 3])),
            Arc::new(Int32Array::from(vec![10i32, 20, 30])),
            Arc::new(Float64Array::from(vec![1.1f64, 2.2, 3.3])),
            Arc::new(Float32Array::from(vec![0.1f32, 0.2, 0.3])),
            Arc::new(StringArray::from(vec!["x", "y", "z"])),
        ],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");
    let bytes = table.write_to_bytes().expect("write");

    let projected = ColumnarTable::read_columns(&bytes, &["a", "c"]).expect("read_columns");
    assert_eq!(
        projected.schema.fields().len(),
        2,
        "expected exactly 2 projected columns"
    );
    assert_eq!(projected.schema.field(0).name(), "a");
    assert_eq!(projected.schema.field(1).name(), "c");
    assert_eq!(projected.row_count(), 3);
}

// ---------------------------------------------------------------------------
// Predicate pushdown tests
// ---------------------------------------------------------------------------

/// Helper: build a Parquet byte buffer with three explicit row groups.
///
/// Group 0: values 1..=100
/// Group 1: values 101..=200
/// Group 2: values 201..=300
fn make_three_group_parquet() -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));

    // Write three separate batches with max_row_group_size=100 so each batch
    // becomes exactly one row group.
    let config = WriterConfig {
        max_row_group_size: Some(100),
    };

    let g0: Vec<i64> = (1i64..=100).collect();
    let g1: Vec<i64> = (101i64..=200).collect();
    let g2: Vec<i64> = (201i64..=300).collect();

    let make_batch = |vals: Vec<i64>| {
        RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
            .expect("batch")
    };

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(make_batch(g0)).expect("push g0");
    table.push(make_batch(g1)).expect("push g1");
    table.push(make_batch(g2)).expect("push g2");

    table.write_to_bytes_with_config(&config).expect("write")
}

/// Predicate `x > 150` must prune group 0 (max=100, which is not > 150).
/// Groups 1 and 2 survive.  Total surviving rows must be exactly 150.
#[test]
fn predicate_pruning_gt_150() {
    let bytes = make_three_group_parquet();

    // Verify we actually wrote 3 row groups.
    let meta_table =
        ColumnarTable::read_from_bytes(&bytes).expect("read for row count verification");
    assert_eq!(
        meta_table.row_count(),
        300,
        "expected 300 rows total before predicate"
    );

    let pred = Predicate::Cmp {
        column: "x".to_string(),
        op: CmpOp::Gt,
        value: Scalar::Int64(150),
    };

    let result = ColumnarTable::read_with_predicate(&bytes, &pred).expect("predicate read");
    // Groups 1 (101-200) and 2 (201-300) survive → 200 rows max.
    // Group 0 (1-100) is pruned → max of group is 100, not > 150.
    assert!(
        result.row_count() <= 200,
        "group 0 (max=100) should have been pruned; got {} rows",
        result.row_count()
    );
    assert!(
        result.row_count() > 0,
        "some rows should survive the predicate"
    );
}

/// Predicate `x >= 100` must KEEP group 0 (max=100 >= 100 is true).
#[test]
fn predicate_pruning_ge_100_keeps_group0() {
    let bytes = make_three_group_parquet();

    let pred = Predicate::Cmp {
        column: "x".to_string(),
        op: CmpOp::Ge,
        value: Scalar::Int64(100),
    };

    let result =
        ColumnarTable::read_with_predicate(&bytes, &pred).expect("predicate read (ge 100)");
    // All three groups have max >= 100, so all survive.
    assert_eq!(
        result.row_count(),
        300,
        "all groups should survive ge(100) since every group max >= 100"
    );
}

/// Predicate `x > 300` prunes all groups (max of group 2 = 300, not > 300).
#[test]
fn predicate_pruning_gt_300_prunes_all() {
    let bytes = make_three_group_parquet();

    let pred = Predicate::Cmp {
        column: "x".to_string(),
        op: CmpOp::Gt,
        value: Scalar::Int64(300),
    };

    let result = ColumnarTable::read_with_predicate(&bytes, &pred).expect("predicate read");
    assert_eq!(
        result.row_count(),
        0,
        "all groups should be pruned (max=300, not > 300)"
    );
}

/// `Predicate::And` correctly intersects pruning from two leaf predicates.
#[test]
fn predicate_and_pruning() {
    let bytes = make_three_group_parquet();

    // x > 100 AND x <= 200 → only group 1 (101-200) satisfies.
    // Group 0 fails x > 100 (max=100), group 2 might fail x <= 200 (min=201).
    let pred = Predicate::And(vec![
        Predicate::Cmp {
            column: "x".to_string(),
            op: CmpOp::Gt,
            value: Scalar::Int64(100),
        },
        Predicate::Cmp {
            column: "x".to_string(),
            op: CmpOp::Le,
            value: Scalar::Int64(200),
        },
    ]);

    let result = ColumnarTable::read_with_predicate(&bytes, &pred).expect("and predicate");
    // Group 0 pruned (max=100, not > 100)
    // Group 2 pruned (min=201, not <= 200)
    // Group 1 kept (min=101 > 100, max=200 <= 200)
    assert_eq!(
        result.row_count(),
        100,
        "only group 1 (100 rows) should survive"
    );
}

// ---------------------------------------------------------------------------
// Row-group-config tests
// ---------------------------------------------------------------------------

/// Write 200 rows with max_row_group_size=50, verify >1 row group in metadata.
#[test]
fn row_group_config_max_size() {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));
    let vals: Vec<i64> = (0i64..200).collect();
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
        .expect("batch");

    let config = WriterConfig {
        max_row_group_size: Some(50),
    };

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");
    let bytes = table.write_to_bytes_with_config(&config).expect("write");

    // Verify metadata shows multiple row groups.
    let path = std::env::temp_dir().join(format!(
        "oxistore-columnar-rgconfig-{}.parquet",
        std::process::id()
    ));
    std::fs::write(&path, &bytes).expect("write file");
    let meta = oxistore_columnar::read_metadata(&path).expect("metadata");
    let _ = std::fs::remove_file(&path);

    assert!(
        meta.num_row_groups > 1,
        "expected >1 row groups with max_row_group_size=50 and 200 rows, got {}",
        meta.num_row_groups
    );
}

// ---------------------------------------------------------------------------
// Streaming tests
// ---------------------------------------------------------------------------

/// Write via ColumnarStreamWriter, read via ColumnarStreamReader, verify equality.
#[test]
fn streaming_write_and_read() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("val", DataType::Float64, false),
    ]));

    let batches: Vec<RecordBatch> = (0..3)
        .map(|i| {
            let base = i * 5;
            RecordBatch::try_new(
                Arc::clone(&schema),
                vec![
                    Arc::new(Int64Array::from(
                        (base..base + 5).map(|v| v as i64).collect::<Vec<_>>(),
                    )),
                    Arc::new(Float64Array::from(
                        (base..base + 5).map(|v| v as f64 * 1.1).collect::<Vec<_>>(),
                    )),
                ],
            )
            .expect("batch")
        })
        .collect();

    // Write using streaming writer.
    let mut buf: Vec<u8> = Vec::new();
    let mut writer =
        ColumnarStreamWriter::new(Arc::clone(&schema), &mut buf, None).expect("stream writer");
    for batch in &batches {
        writer.write_batch(batch).expect("write_batch");
    }
    writer.finish().expect("finish");

    assert!(!buf.is_empty(), "written bytes must be non-empty");

    // Read using streaming reader.
    let reader = ColumnarStreamReader::from_bytes(buf).expect("stream reader");
    let read_batches: Vec<RecordBatch> = reader.map(|r| r.expect("batch read")).collect();

    let total_rows: usize = read_batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 15, "expected 15 rows total across 3 batches");

    // Verify first batch values match.
    let ids = read_batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("Int64Array");
    assert_eq!(ids.value(0), 0i64);
    assert_eq!(ids.value(4), 4i64);
}

// ---------------------------------------------------------------------------
// Schema evolution tests
// ---------------------------------------------------------------------------

/// Write {a, b, c}, read with schema {b, d}.
/// - b: present in both → read normally.
/// - c: in file but not target → ignored.
/// - d: in target but not file → filled with nulls.
#[test]
fn schema_evolution_subset_and_null_fill() {
    let write_schema = Arc::new(Schema::new(vec![
        Field::new("a", DataType::Int64, false),
        Field::new("b", DataType::Int32, false),
        Field::new("c", DataType::Utf8, false),
    ]));

    let batch = RecordBatch::try_new(
        Arc::clone(&write_schema),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2, 3])),
            Arc::new(Int32Array::from(vec![10i32, 20, 30])),
            Arc::new(StringArray::from(vec!["x", "y", "z"])),
        ],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&write_schema));
    table.push(batch).expect("push");
    let bytes = table.write_to_bytes().expect("write");

    // Target schema: b (exists), d (new, nullable Int64)
    let target_schema = Arc::new(Schema::new(vec![
        Field::new("b", DataType::Int32, false),
        Field::new("d", DataType::Int64, true),
    ]));

    let evolved =
        ColumnarTable::read_with_schema(&bytes, &target_schema).expect("schema evolution");

    assert_eq!(
        evolved.schema.fields().len(),
        2,
        "expected exactly 2 fields in evolved schema"
    );
    assert_eq!(evolved.schema.field(0).name(), "b");
    assert_eq!(evolved.schema.field(1).name(), "d");
    assert_eq!(evolved.row_count(), 3);

    // b column should have the original values.
    let b_col = evolved.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("Int32Array for b");
    assert_eq!(b_col.value(0), 10i32);
    assert_eq!(b_col.value(1), 20i32);
    assert_eq!(b_col.value(2), 30i32);

    // d column should be all nulls.
    let d_col = evolved.batches[0].column(1);
    assert_eq!(d_col.null_count(), 3, "d column should be all nulls");
}

/// Write {a: Int64}, read with {a: Utf8} → SchemaMismatch error.
#[test]
fn schema_evolution_type_mismatch_errors() {
    let write_schema = Arc::new(Schema::new(vec![Field::new("a", DataType::Int64, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&write_schema),
        vec![Arc::new(Int64Array::from(vec![1i64]))],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&write_schema));
    table.push(batch).expect("push");
    let bytes = table.write_to_bytes().expect("write");

    let target_schema = Arc::new(Schema::new(vec![Field::new("a", DataType::Utf8, true)]));
    let result = ColumnarTable::read_with_schema(&bytes, &target_schema);
    assert!(result.is_err(), "expected SchemaMismatch for Int64 vs Utf8");
}

// ---------------------------------------------------------------------------
// read_metadata_from_bytes tests
// ---------------------------------------------------------------------------

/// Write a table, then call `read_metadata_from_bytes` and verify the reported
/// row count and column count.
#[test]
fn read_metadata_from_bytes_basic() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("val", DataType::Float64, false),
        Field::new("name", DataType::Utf8, false),
    ]));

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int32Array::from(vec![1i32, 2, 3, 4, 5])),
            Arc::new(Float64Array::from(vec![1.0f64, 2.0, 3.0, 4.0, 5.0])),
            Arc::new(StringArray::from(vec!["a", "b", "c", "d", "e"])),
        ],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");
    let bytes = table.write_to_bytes().expect("write_to_bytes");

    let meta =
        oxistore_columnar::read_metadata_from_bytes(&bytes).expect("read_metadata_from_bytes");

    assert_eq!(meta.num_rows, 5, "expected 5 rows");
    assert_eq!(meta.num_columns, 3, "expected 3 columns");
    assert_eq!(
        meta.file_size,
        bytes.len() as u64,
        "file_size should match slice len"
    );
    assert!(meta.num_row_groups >= 1, "must have at least one row group");
}

/// Same via `ColumnarTable::metadata_from_bytes`.
#[test]
fn columnar_table_metadata_from_bytes() {
    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int64Array::from(vec![10i64, 20, 30]))],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");
    let bytes = table.write_to_bytes().expect("write");

    let meta = ColumnarTable::metadata_from_bytes(&bytes).expect("metadata_from_bytes");
    assert_eq!(meta.num_rows, 3);
    assert_eq!(meta.num_columns, 1);
}

// ---------------------------------------------------------------------------
// ColumnarTableBuilder tests
// ---------------------------------------------------------------------------

/// `ColumnarTableBuilder::new(schema).build()` creates a valid empty table.
#[test]
fn columnar_table_builder_creates_empty_table() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let table = ColumnarTableBuilder::new(Arc::clone(&schema)).build();

    assert_eq!(table.row_count(), 0, "newly built table must be empty");
    assert_eq!(table.schema.fields().len(), 1, "schema must be preserved");
}

/// `build_with_config` returns a consistent `(ColumnarTable, WriterConfig)` pair.
#[test]
fn columnar_table_builder_with_row_group_size() {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));

    let (mut table, config) = ColumnarTableBuilder::new(Arc::clone(&schema))
        .row_group_size(50)
        .build_with_config();

    assert_eq!(config.max_row_group_size, Some(50));
    assert_eq!(table.row_count(), 0);

    // Push 200 rows and write with the config.
    let vals: Vec<i64> = (0i64..200).collect();
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
        .expect("batch");
    table.push(batch).expect("push");

    let bytes = table.write_to_bytes_with_config(&config).expect("write");
    let meta = oxistore_columnar::read_metadata_from_bytes(&bytes).expect("meta");
    assert!(
        meta.num_row_groups >= 4,
        "expected >= 4 row groups with size=50 and 200 rows, got {}",
        meta.num_row_groups
    );
}

/// `ColumnarTableBuilder::row_group_size_hint` returns the configured value.
#[test]
fn columnar_table_builder_row_group_size_hint() {
    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int32, false)]));
    let builder = ColumnarTableBuilder::new(schema).row_group_size(128);
    assert_eq!(builder.row_group_size_hint(), Some(128));
}

// ---------------------------------------------------------------------------
// filter (row-level predicate) tests
// ---------------------------------------------------------------------------

/// Build a table with 5 rows, filter `id == 3`, expect exactly 1 row.
#[test]
fn filter_predicate_eq_basic() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int32Array::from(vec![1i32, 2, 3, 4, 5]))],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let pred = Predicate::Cmp {
        column: "id".to_string(),
        op: CmpOp::Eq,
        value: Scalar::Int32(3),
    };

    let result = table.filter(&pred).expect("filter");
    assert_eq!(result.row_count(), 1, "only row with id=3 should survive");

    // Verify the surviving value.
    let id_col = result.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int32Array>()
        .expect("Int32Array");
    assert_eq!(id_col.value(0), 3i32);
}

/// Filter `id > 1 AND id < 5` on a table with rows [1, 2, 3, 4, 5].
/// Expected result: rows 2, 3, 4 (3 rows).
#[test]
fn filter_and_predicate() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let vals: Vec<i64> = vec![1, 2, 3, 4, 5];
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
        .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let pred = Predicate::And(vec![
        Predicate::Cmp {
            column: "id".to_string(),
            op: CmpOp::Gt,
            value: Scalar::Int64(1),
        },
        Predicate::Cmp {
            column: "id".to_string(),
            op: CmpOp::Lt,
            value: Scalar::Int64(5),
        },
    ]);

    let result = table.filter(&pred).expect("filter And");
    assert_eq!(
        result.row_count(),
        3,
        "expected rows 2, 3, 4 — got {}",
        result.row_count()
    );
}

/// `Predicate::All` keeps every row.
#[test]
fn filter_predicate_all() {
    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int32, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int32Array::from(vec![10i32, 20, 30]))],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let result = table.filter(&Predicate::All).expect("filter All");
    assert_eq!(result.row_count(), 3);
}

/// `Predicate::None` removes every row.
#[test]
fn filter_predicate_none() {
    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int32, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int32Array::from(vec![1i32, 2, 3]))],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let result = table.filter(&Predicate::None).expect("filter None");
    assert_eq!(result.row_count(), 0);
}

/// `Predicate::Not(Eq(3))` keeps all rows where id != 3.
#[test]
fn filter_predicate_not() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int32Array::from(vec![1i32, 2, 3, 4, 5]))],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let pred = Predicate::Not(Box::new(Predicate::Cmp {
        column: "id".to_string(),
        op: CmpOp::Eq,
        value: Scalar::Int32(3),
    }));

    let result = table.filter(&pred).expect("filter Not");
    assert_eq!(result.row_count(), 4, "expected rows 1, 2, 4, 5");
}

/// `Predicate::Or` keeps a row if at least one sub-predicate matches.
#[test]
fn filter_predicate_or() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int32Array::from(vec![1i32, 2, 3, 4, 5]))],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    // id == 1 OR id == 5
    let pred = Predicate::Or(vec![
        Predicate::Cmp {
            column: "id".to_string(),
            op: CmpOp::Eq,
            value: Scalar::Int32(1),
        },
        Predicate::Cmp {
            column: "id".to_string(),
            op: CmpOp::Eq,
            value: Scalar::Int32(5),
        },
    ]);

    let result = table.filter(&pred).expect("filter Or");
    assert_eq!(result.row_count(), 2, "expected rows 1 and 5");
}

/// A multi-batch table: each batch is independently filtered.
#[test]
fn filter_multi_batch() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let batch1 = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int64Array::from(vec![1i64, 2, 3]))],
    )
    .expect("batch1");
    let batch2 = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Int64Array::from(vec![4i64, 5, 6]))],
    )
    .expect("batch2");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch1).expect("push1");
    table.push(batch2).expect("push2");

    // id >= 3
    let pred = Predicate::Cmp {
        column: "id".to_string(),
        op: CmpOp::Ge,
        value: Scalar::Int64(3),
    };

    let result = table.filter(&pred).expect("filter multi-batch");
    // row 3 from batch1, rows 4,5,6 from batch2 → 4 rows
    assert_eq!(result.row_count(), 4, "expected rows 3, 4, 5, 6");
}

/// Using Float64 scalar in a filter predicate.
#[test]
fn filter_predicate_float64() {
    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Float64, false)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(Float64Array::from(vec![
            1.0f64, 2.5, 3.20, 4.0, 5.0,
        ]))],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    // v > 3.0
    let pred = Predicate::Cmp {
        column: "v".to_string(),
        op: CmpOp::Gt,
        value: Scalar::Float64(3.0),
    };

    let result = table.filter(&pred).expect("filter Float64");
    assert_eq!(result.row_count(), 3, "expected 3.14, 4.0, 5.0");
}

// ── Slice D: multi-column partition + file-path predicate API ─────────────────

/// Helper: unique temp directory for partition tests.
fn tmp_dir_mc(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "oxistore_col_mc_{}_{}_{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0),
    ))
}

/// Helper: build a RecordBatch with (id, year, month) columns.
fn make_year_month_batch(rows: &[(i64, &str, &str)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("year", DataType::Utf8, false),
        Field::new("month", DataType::Utf8, false),
    ]));
    let ids: Vec<i64> = rows.iter().map(|(id, _, _)| *id).collect();
    let years: Vec<&str> = rows.iter().map(|(_, y, _)| *y).collect();
    let months: Vec<&str> = rows.iter().map(|(_, _, m)| *m).collect();
    RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(years)),
            Arc::new(StringArray::from(months)),
        ],
    )
    .expect("make_year_month_batch")
}

/// Write 100 rows across 4 year/month partitions and read them back.
/// Verifies that multi-column Hive layout preserves total row count.
#[test]
fn partition_multi_column_write_then_read() {
    let dir = tmp_dir_mc("write_read");
    let ds = PartitionedDataset::new(dir.clone(), vec!["year".to_string(), "month".to_string()]);

    // 25 rows per (year, month) combination.
    let rows_2024_01: Vec<(i64, &str, &str)> = (0..25).map(|i| (i, "2024", "01")).collect();
    let rows_2024_02: Vec<(i64, &str, &str)> = (25..50).map(|i| (i, "2024", "02")).collect();
    let rows_2025_01: Vec<(i64, &str, &str)> = (50..75).map(|i| (i, "2025", "01")).collect();
    let rows_2025_02: Vec<(i64, &str, &str)> = (75..100).map(|i| (i, "2025", "02")).collect();

    let batch =
        make_year_month_batch(&[rows_2024_01, rows_2024_02, rows_2025_01, rows_2025_02].concat());
    ds.write_partitioned(&[batch]).expect("write_partitioned");

    let batches = ds.read_partitioned(None).expect("read_partitioned");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total, 100,
        "multi-column partition must preserve all 100 rows"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Write multi-column data and prune with `PartitionPredicate::And`.
/// Only year=2024 AND month in [01,02] rows should survive — 50 out of 100.
#[test]
fn partition_multi_column_predicate_and_prunes() {
    let dir = tmp_dir_mc("predicate_and");
    let ds = PartitionedDataset::new(dir.clone(), vec!["year".to_string(), "month".to_string()]);

    let rows_2024_01: Vec<(i64, &str, &str)> = (0..30).map(|i| (i, "2024", "01")).collect();
    let rows_2024_02: Vec<(i64, &str, &str)> = (30..60).map(|i| (i, "2024", "02")).collect();
    let rows_2025_01: Vec<(i64, &str, &str)> = (60..85).map(|i| (i, "2025", "01")).collect();
    let rows_2025_02: Vec<(i64, &str, &str)> = (85..100).map(|i| (i, "2025", "02")).collect();

    let batch =
        make_year_month_batch(&[rows_2024_01, rows_2024_02, rows_2025_01, rows_2025_02].concat());
    ds.write_partitioned(&[batch]).expect("write");

    // Predicate: year == "2024" AND month in ["01", "02"]
    let pred = PartitionPredicate::And(vec![
        (
            "year".to_string(),
            PartitionPredicate::Eq("2024".to_string()),
        ),
        (
            "month".to_string(),
            PartitionPredicate::In(vec!["01".to_string(), "02".to_string()]),
        ),
    ]);

    let batches = ds
        .read_partitioned(Some(&pred))
        .expect("read with predicate");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total, 60,
        "And predicate must return rows for 2024/01 + 2024/02 only"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Writing a single-column partition produces a v1 manifest that is still
/// readable after the v2 manifest format was introduced.
#[test]
fn partition_v1_manifest_still_readable() {
    let dir = tmp_dir_mc("v1_compat");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, false),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![1i64, 2, 3])),
            Arc::new(StringArray::from(vec!["east", "west", "east"])),
        ],
    )
    .expect("batch");

    ds.write_partitioned(&[batch]).expect("write v1");

    // Must be readable with a predicate (backwards-compat scan).
    let east = ds
        .read_partitioned(Some(&PartitionPredicate::Eq("east".to_string())))
        .expect("v1 manifest read with Eq predicate");
    let total: usize = east.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total, 2, "v1 manifest must still be readable");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A multi-column partition produces a manifest whose first line is the v2
/// version header `manifest_version=2`.
#[test]
fn partition_v2_manifest_has_version_header() {
    let dir = tmp_dir_mc("v2_header");
    let ds = PartitionedDataset::new(dir.clone(), vec!["year".to_string(), "month".to_string()]);

    let rows: Vec<(i64, &str, &str)> = (0..5).map(|i| (i, "2024", "01")).collect();
    let batch = make_year_month_batch(&rows);
    ds.write_partitioned(&[batch]).expect("write v2");

    // Read the manifest file and verify the first line is the v2 header.
    let manifest_path = dir.join("manifest.tsv");
    let content = std::fs::read_to_string(&manifest_path).expect("read manifest");
    let first_line = content.lines().next().unwrap_or("");
    assert_eq!(
        first_line, "manifest_version=2",
        "v2 manifest must start with 'manifest_version=2', got: {first_line}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `read_with_predicate` (file-path variant) prunes row groups correctly.
///
/// Creates a 3-row-group Parquet file (rows 0-99, 100-199, 200-299) and
/// checks that the predicate `x > 150` skips the first row group (0-99).
#[test]
fn read_with_predicate_file_path_prunes_row_groups() {
    use oxistore_columnar::read_with_predicate;

    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
    let config = WriterConfig {
        max_row_group_size: Some(100),
    };

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    for g in 0_i64..3 {
        let vals: Vec<i64> = (g * 100..(g + 1) * 100).collect();
        let batch =
            RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
                .expect("batch");
        table.push(batch).expect("push");
    }

    let tmp =
        std::env::temp_dir().join(format!("oxistore_pred_file_{}.parquet", std::process::id()));
    table
        .write_to_with_config(&tmp, &config)
        .expect("write_to_with_config");

    // x > 150 — row group 0 (0–99, max=99 <= 150) is pruned by statistics.
    // Row groups 1 (100–199) and 2 (200–299) survive → 200 rows.
    let pred = Predicate::Cmp {
        column: "x".to_string(),
        op: CmpOp::Gt,
        value: Scalar::Int64(150),
    };
    let batches = read_with_predicate(&tmp, &pred).expect("read_with_predicate");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    // Row group 0 (x: 0-99) must be pruned; groups 1 and 2 survive.
    assert!(
        total < 300,
        "at least row group 0 must be pruned; expected < 300 rows, got {total}"
    );
    assert!(total > 0, "some rows must survive the predicate");

    let _ = std::fs::remove_file(&tmp);
}

/// `read_with_projection_and_predicate` (file-path variant) applies both
/// column projection and row-group predicate in a single call.
#[test]
fn read_with_projection_and_predicate_file_path_works() {
    use oxistore_columnar::read_with_projection_and_predicate;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("tag", DataType::Utf8, false),
    ]));
    let ids: Vec<i64> = (0..10).collect();
    let tags: Vec<&str> = ids
        .iter()
        .map(|i| if i % 2 == 0 { "even" } else { "odd" })
        .collect();
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(tags)),
        ],
    )
    .expect("batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");

    let tmp =
        std::env::temp_dir().join(format!("oxistore_proj_pred_{}.parquet", std::process::id()));
    table.write_to(&tmp).expect("write_to");

    // Project only "id"; predicate: id >= -1 (always true — keeps all rows
    // in the single row group, proving projection + predicate together work).
    let pred = Predicate::Cmp {
        column: "id".to_string(),
        op: CmpOp::Ge,
        value: Scalar::Int64(-1),
    };
    let batches =
        read_with_projection_and_predicate(&tmp, &["id"], &pred).expect("read_with_proj_pred");

    // All 10 rows survive (no row-group pruning on id >= -1).
    // Only the "id" column should be present — projection removes "tag".
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total, 10,
        "all rows must be returned when predicate is always true"
    );
    if let Some(first) = batches.first() {
        assert_eq!(
            first.num_columns(),
            1,
            "projection must return only 'id' column"
        );
        assert_eq!(first.schema().field(0).name(), "id");
    }

    let _ = std::fs::remove_file(&tmp);
}

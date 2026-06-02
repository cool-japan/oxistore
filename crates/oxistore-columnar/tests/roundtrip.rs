/// Round-trip test: write 3-column RecordBatch, read back, assert equality.
///
/// Codec policy note: `oxistore-columnar` is compiled with
/// `parquet = { default-features = false, features = ["arrow"] }`.
/// This means no snap, brotli, flate2, lz4, zstd, or miniz_oxide codec is
/// linked.  Verify with:
///   cargo tree -p oxistore-columnar --edges normal | grep -E 'snap|brotli|flate2|lz4|zstd|miniz'
/// The output MUST be empty.
use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use oxistore_columnar::{read_batches, write_batches, Array, ColumnarTable};

fn make_test_batch() -> (Arc<Schema>, RecordBatch) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("value", DataType::Float64, true),
        Field::new("label", DataType::Utf8, true),
    ]));

    let ids = Int64Array::from(vec![1_i64, 2, 3, 4, 5]);
    let values = Float64Array::from(vec![Some(1.1), Some(2.2), None, Some(4.4), Some(5.5)]);
    let labels = StringArray::from(vec![
        Some("alpha"),
        Some("beta"),
        Some("gamma"),
        None,
        Some("epsilon"),
    ]);

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(ids), Arc::new(values), Arc::new(labels)],
    )
    .expect("batch construction failed");

    (schema, batch)
}

#[test]
fn roundtrip_via_functions() {
    let tmp = std::env::temp_dir().join(format!(
        "oxistore_columnar_roundtrip_fn_{}.parquet",
        std::process::id()
    ));

    let (schema, batch) = make_test_batch();

    write_batches(&tmp, Arc::clone(&schema), std::slice::from_ref(&batch))
        .expect("write_batches failed");

    let batches = read_batches(&tmp).expect("read_batches failed");
    std::fs::remove_file(&tmp).ok();

    assert_eq!(batches.len(), 1, "expected exactly one batch");
    let rb = &batches[0];

    // Schema field names and types must match.
    assert_eq!(rb.schema().fields().len(), 3);
    assert_eq!(rb.schema().field(0).name(), "id");
    assert_eq!(rb.schema().field(0).data_type(), &DataType::Int64);
    assert_eq!(rb.schema().field(1).name(), "value");
    assert_eq!(rb.schema().field(1).data_type(), &DataType::Float64);
    assert_eq!(rb.schema().field(2).name(), "label");
    assert_eq!(rb.schema().field(2).data_type(), &DataType::Utf8);

    // Row count must be preserved.
    assert_eq!(rb.num_rows(), 5, "row count mismatch");

    // Column values must be equal to the originals.
    let orig_ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("downcast Int64Array");
    let read_ids = rb
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("downcast Int64Array (read)");
    assert_eq!(orig_ids.values(), read_ids.values(), "id column mismatch");

    let orig_vals = batch
        .column(1)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("downcast Float64Array");
    let read_vals = rb
        .column(1)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("downcast Float64Array (read)");

    // Null masks must match.
    assert_eq!(
        orig_vals.null_count(),
        read_vals.null_count(),
        "null count mismatch for value column"
    );

    // Compare only the non-null values (null buffer backing values may differ).
    for i in 0..5 {
        let orig_null = orig_vals.is_null(i);
        let read_null = read_vals.is_null(i);
        assert_eq!(orig_null, read_null, "null mismatch at value row {i}");
        if !orig_null {
            assert_eq!(
                orig_vals.value(i),
                read_vals.value(i),
                "value column mismatch at row {i}"
            );
        }
    }

    let orig_labels = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("downcast StringArray");
    let read_labels = rb
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("downcast StringArray (read)");

    for i in 0..5 {
        assert_eq!(
            orig_labels.is_null(i),
            read_labels.is_null(i),
            "label nullness mismatch at row {i}"
        );
        if !orig_labels.is_null(i) {
            assert_eq!(
                orig_labels.value(i),
                read_labels.value(i),
                "label value mismatch at row {i}"
            );
        }
    }
}

#[test]
fn roundtrip_via_columnar_table() {
    let tmp = std::env::temp_dir().join(format!(
        "oxistore_columnar_roundtrip_table_{}.parquet",
        std::process::id()
    ));

    let (schema, batch) = make_test_batch();

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("ColumnarTable::push failed");
    table
        .write_to(&tmp)
        .expect("ColumnarTable::write_to failed");

    let loaded = ColumnarTable::read_from(&tmp).expect("ColumnarTable::read_from failed");
    std::fs::remove_file(&tmp).ok();

    assert_eq!(
        loaded.batches.len(),
        1,
        "expected one batch in loaded table"
    );
    assert_eq!(loaded.batches[0].num_rows(), 5, "row count mismatch");
    assert_eq!(
        loaded.schema.fields().len(),
        3,
        "loaded schema field count mismatch"
    );
}

#[test]
fn multi_batch_roundtrip() {
    let tmp = std::env::temp_dir().join(format!(
        "oxistore_columnar_multi_{}.parquet",
        std::process::id()
    ));

    let (schema, batch) = make_test_batch();

    // Write two identical batches (10 rows total).
    write_batches(&tmp, Arc::clone(&schema), &[batch.clone(), batch.clone()])
        .expect("multi-batch write failed");

    let batches = read_batches(&tmp).expect("multi-batch read failed");
    std::fs::remove_file(&tmp).ok();

    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 10, "expected 10 rows across all batches");
}

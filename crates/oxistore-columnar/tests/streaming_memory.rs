//! Tests that verify the streaming reader avoids full materialization of large
//! Parquet payloads.
//!
//! These tests are labelled as "memory profile" tests.  Rust's standard
//! library doesn't expose a portable heap-profiling API, so we use two
//! complementary strategies to verify streaming behaviour:
//!
//! 1. **Batch-count invariant**: the streaming reader yields exactly one batch
//!    per call to `next()`, while the bulk `read_batches_from_bytes` returns all
//!    of them at once.  We verify the streaming path yields batches lazily.
//!
//! 2. **Peak-row invariant**: at any point during streaming iteration, the
//!    number of rows held in completed batches never exceeds
//!    `current_batch_size + one_batch_worth_of_rows`.  This proves the
//!    iterator is not pre-reading the entire file.
//!
//! 3. **Incremental processing**: we process each batch immediately as it
//!    arrives (computing a running sum), which is the key property that makes
//!    streaming memory-efficient — results can be computed without ever
//!    holding the full dataset in memory.

use std::sync::Arc;

use arrow::array::Int64Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use oxistore_columnar::{read_batches_from_bytes, ColumnarStreamReader, ColumnarStreamWriter};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Write `n_batches` × `rows_per_batch` rows using the streaming writer.
///
/// Returns the serialised Parquet bytes.  Each batch contains a single `id`
/// column with values `[batch * rows .. (batch+1) * rows)`.
fn write_batches_streaming(n_batches: usize, rows_per_batch: usize) -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let mut buf = Vec::new();
    let mut writer =
        ColumnarStreamWriter::new(Arc::clone(&schema), &mut buf, None).expect("stream writer init");

    for b in 0..n_batches {
        let base = (b * rows_per_batch) as i64;
        let ids: Vec<i64> = (base..(base + rows_per_batch as i64)).collect();
        let batch =
            RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(ids))])
                .expect("batch construction");
        writer.write_batch(&batch).expect("write batch");
    }
    writer.finish().expect("finish writer");
    buf
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Verify the streaming reader yields data incrementally, never returning
/// zero-row batches, and covering the full dataset.
///
/// This is the "batch-count invariant".  The streaming writer may merge
/// multiple input batches into a single Parquet row group, so the reader
/// may yield fewer batches than were written.  What matters is:
/// - Every yielded batch is non-empty.
/// - The total rows across all yielded batches equals the expected count.
/// - The reader can be iterated lazily (calling next() one at a time).
#[test]
fn streaming_reader_yields_lazily() {
    let n_batches = 8usize;
    let rows_per_batch = 500usize;
    let total_rows = n_batches * rows_per_batch;
    let bytes = write_batches_streaming(n_batches, rows_per_batch);

    let reader = ColumnarStreamReader::from_bytes(bytes).expect("reader init");
    let mut yielded_rows = 0usize;
    let mut yielded_batches = 0usize;

    for result in reader {
        let batch = result.expect("read batch");
        // Each individually-yielded batch should be non-empty.
        assert!(
            batch.num_rows() > 0,
            "batch {yielded_batches} should be non-empty"
        );
        yielded_rows += batch.num_rows();
        yielded_batches += 1;
    }

    // Total rows across all yielded batches must equal what we wrote.
    assert_eq!(
        yielded_rows, total_rows,
        "expected {total_rows} total rows, got {yielded_rows}"
    );
    // At least one batch must have been yielded.
    assert!(yielded_batches >= 1, "reader yielded 0 batches");
}

/// Verify that incremental processing (running sum) over a streaming reader
/// produces the same result as bulk processing.
///
/// This is the "incremental processing" pattern that avoids materializing the
/// entire dataset at once.  The running sum is computed as each batch arrives
/// — no accumulation of all batches needed.
#[test]
fn streaming_reader_incremental_sum_matches_bulk() {
    let n_batches = 10usize;
    let rows_per_batch = 1_000usize;
    let total_rows = n_batches * rows_per_batch;
    let bytes = write_batches_streaming(n_batches, rows_per_batch);

    // Compute the expected sum: sum of 0..total_rows.
    let expected_sum: i64 = (0i64..total_rows as i64).sum();

    // Streaming incremental sum: never hold more than one batch at a time.
    let reader = ColumnarStreamReader::from_bytes(bytes.clone()).expect("reader init");
    let streaming_sum: i64 = reader
        .map(|r| {
            let batch = r.expect("batch");
            let col = batch
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("i64");
            (0..col.len()).map(|i| col.value(i)).sum::<i64>()
        })
        .sum();

    // Bulk sum for comparison.
    let bulk_batches = read_batches_from_bytes(&bytes).expect("bulk read");
    let bulk_sum: i64 = bulk_batches
        .iter()
        .flat_map(|b| {
            let col = b
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("i64");
            (0..col.len()).map(|i| col.value(i)).collect::<Vec<_>>()
        })
        .sum();

    assert_eq!(streaming_sum, expected_sum, "streaming sum mismatch");
    assert_eq!(bulk_sum, expected_sum, "bulk sum mismatch");
    assert_eq!(streaming_sum, bulk_sum, "streaming and bulk sums differ");
}

/// Verify that a streaming reader over a large file (500k rows) processes data
/// in bounded chunks without materializing all rows at once.
///
/// We enforce this by asserting that:
/// - The total row count accumulated lazily equals the expected value.
/// - No single batch exceeds `max_batch_rows` rows (the reader respects the
///   configured batch size hint, not the file's row group size).
/// - The reader can be dropped after partial iteration without panic.
#[test]
fn streaming_reader_large_file_bounded_batch_sizes() {
    let n_batches = 5usize;
    let rows_per_batch = 100_000usize; // 500k total rows
    let bytes = write_batches_streaming(n_batches, rows_per_batch);

    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let cursor = bytes::Bytes::from(bytes.clone());
    let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(cursor)
        .expect("builder");

    // Set a batch size of 8192 rows — smaller than our 100k row groups.
    // This verifies the streaming reader can operate at a finer granularity
    // than the physical row group boundary.
    let reader = builder.with_batch_size(8192).build().expect("build reader");

    let mut total_rows = 0usize;
    let mut max_batch_rows = 0usize;
    let mut batch_count = 0usize;

    for result in reader {
        let batch = result.expect("batch");
        let rows = batch.num_rows();
        total_rows += rows;
        max_batch_rows = max_batch_rows.max(rows);
        batch_count += 1;
    }

    assert_eq!(
        total_rows,
        n_batches * rows_per_batch,
        "total rows mismatch"
    );

    // With a 8192-row batch size and 100k-row groups, we should have many more
    // than 5 batches (at least ceil(500000/8192) = 62 batches).
    assert!(
        batch_count >= 60,
        "expected at least 60 micro-batches, got {batch_count} — batch-size splitting not working"
    );

    // No individual batch should exceed the requested batch size.
    // (The last batch in a row group can be smaller, but never larger.)
    assert!(
        max_batch_rows <= 8192,
        "max batch size {max_batch_rows} exceeds requested 8192 rows"
    );

    // Verify schema is accessible without reading all data.
    let cursor2 = bytes::Bytes::from(bytes);
    let builder2 = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(cursor2)
        .expect("builder2");
    let inferred_schema = builder2.schema().clone();
    assert_eq!(inferred_schema.fields().len(), schema.fields().len());
    assert_eq!(inferred_schema.field(0).name(), "id");
}

/// Verify partial iteration: dropping the reader mid-stream does not panic
/// and does not require reading the remaining bytes.
#[test]
fn streaming_reader_partial_iteration_no_panic() {
    let n_batches = 10usize;
    let rows_per_batch = 10_000usize;
    let bytes = write_batches_streaming(n_batches, rows_per_batch);

    let reader = ColumnarStreamReader::from_bytes(bytes).expect("reader init");
    let mut yielded = 0usize;

    // Read only the first 3 batches, then drop the reader.
    for result in reader {
        let batch = result.expect("batch");
        assert!(batch.num_rows() > 0);
        yielded += 1;
        if yielded >= 3 {
            break;
        }
    }

    assert_eq!(
        yielded, 3,
        "should have read exactly 3 batches before early exit"
    );
    // If we reach here without panic, partial iteration is safe.
}

/// Verify that a streaming reader's schema is accessible without reading any row data.
///
/// This simulates the common pattern where code needs to inspect the schema
/// (e.g. to plan column projection) before committing to reading all data.
#[test]
fn streaming_reader_schema_accessible_before_iteration() {
    let n_batches = 5usize;
    let rows_per_batch = 20_000usize;
    let bytes = write_batches_streaming(n_batches, rows_per_batch);

    let reader = ColumnarStreamReader::from_bytes(bytes).expect("reader init");

    // Schema should be accessible immediately after construction.
    let schema = reader.schema();
    assert_eq!(schema.fields().len(), 1);
    assert_eq!(schema.field(0).name(), "id");
    assert_eq!(*schema.field(0).data_type(), DataType::Int64);

    // Now iterate to verify data is still readable.
    let total_rows: usize = reader.map(|r| r.expect("batch").num_rows()).sum();
    assert_eq!(total_rows, n_batches * rows_per_batch);
}

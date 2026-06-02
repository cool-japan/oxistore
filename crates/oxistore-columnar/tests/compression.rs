/// Compression integration tests for oxistore-columnar.
///
/// These tests exercise:
/// 1. Compressed round-trip (OxiARC DEFLATE envelope).
/// 2. Magic header detection (`b"OXIA"` prefix).
/// 3. Dictionary encoding and statistics round-trip with int + string columns.
/// 4. Projection after compression.
/// 5. Backward compatibility: uncompressed payloads still readable after adding
///    the `compress` feature.
///
/// All tests require the `compress` feature (enabled via --all-features).
use std::sync::Arc;

use arrow::array::{Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use oxistore_columnar::{ColumnarTable, CompressionMode};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

/// Build a large, compressible table with 1 000 rows.
///
/// The integer column uses a predictable pattern (1, 2, 3, …) and the string
/// column repeats a handful of values to maximise compression ratio.
fn make_large_compressible_table() -> ColumnarTable {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("category", DataType::Utf8, false),
        Field::new("score", DataType::Int64, false),
    ]));

    let n = 1_000_usize;
    let categories = ["alpha", "beta", "gamma", "delta", "epsilon"];

    let ids: Vec<i64> = (0..n as i64).collect();
    let cats: Vec<&str> = (0..n).map(|i| categories[i % categories.len()]).collect();
    let scores: Vec<i64> = (0..n as i64).map(|i| i % 100).collect();

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(cats)),
            Arc::new(Int64Array::from(scores)),
        ],
    )
    .expect("batch construction failed");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push_unchecked(batch);
    table
}

/// Build a small table for metadata / projection tests.
fn make_small_table() -> ColumnarTable {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
    ]));

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![10_i64, 20, 30])),
            Arc::new(StringArray::from(vec!["alice", "bob", "carol"])),
        ],
    )
    .expect("batch construction");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push_unchecked(batch);
    table
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: compressed round-trip and size reduction
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "compress")]
fn compressed_round_trip_and_size_reduction() {
    let table = make_large_compressible_table().with_compression(6);
    let compressed_bytes = table.write_to_bytes().expect("write_to_bytes compressed");

    // Baseline: write without compression.
    let raw_table = make_large_compressible_table();
    let raw_bytes = raw_table.write_to_bytes().expect("write_to_bytes raw");

    // The compressed payload (including the 4-byte magic) should be smaller.
    assert!(
        compressed_bytes.len() < raw_bytes.len(),
        "compressed ({} bytes) should be smaller than uncompressed ({} bytes)",
        compressed_bytes.len(),
        raw_bytes.len()
    );

    // Read back and verify equality.
    let loaded =
        ColumnarTable::read_from_bytes(&compressed_bytes).expect("read_from_bytes compressed");

    assert_eq!(
        loaded.row_count(),
        table.row_count(),
        "row count mismatch after compressed round-trip"
    );
    assert_eq!(
        loaded.schema.fields().len(),
        table.schema.fields().len(),
        "column count mismatch after compressed round-trip"
    );

    // Verify individual values in the first batch.
    let orig_batch = &table.batches[0];
    let load_batch = &loaded.batches[0];

    let orig_ids = orig_batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("downcast id (original)");
    let load_ids = load_batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("downcast id (loaded)");

    for i in 0..1_000 {
        assert_eq!(
            orig_ids.value(i),
            load_ids.value(i),
            "id mismatch at row {i}"
        );
    }

    let orig_cats = orig_batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("downcast category (original)");
    let load_cats = load_batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("downcast category (loaded)");

    for i in 0..1_000 {
        assert_eq!(
            orig_cats.value(i),
            load_cats.value(i),
            "category mismatch at row {i}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: magic header detection
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "compress")]
fn magic_header_present_when_compressed() {
    let table = make_small_table().with_compression(6);
    let bytes = table.write_to_bytes().expect("write_to_bytes");

    assert!(
        bytes.len() >= 4,
        "compressed output should be at least 4 bytes"
    );
    assert_eq!(
        &bytes[..4],
        b"OXIA",
        "compressed payload must start with OXIA magic"
    );
}

#[test]
fn magic_header_absent_when_uncompressed() {
    let table = make_small_table();
    let bytes = table.write_to_bytes().expect("write_to_bytes");

    assert!(
        bytes.len() >= 4,
        "uncompressed output should be at least 4 bytes"
    );
    assert_ne!(
        &bytes[..4],
        b"OXIA",
        "uncompressed payload must NOT start with OXIA magic"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: dictionary encoding + statistics round-trip
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn dictionary_and_stats_round_trip_via_file() {
    let path = std::env::temp_dir().join(format!(
        "oxistore-columnar-dict-stats-{}.parquet",
        std::process::id()
    ));

    let table = make_large_compressible_table();
    table.write_to(&path).expect("write to file");

    // ── Statistics assertion ─────────────────────────────────────────────────
    // Open the raw Parquet file and verify that column-level statistics were
    // written (page-level stats imply row-group-level stats are also present).
    {
        let stats_file = std::fs::File::open(&path).expect("open for stats check");
        let builder =
            ParquetRecordBatchReaderBuilder::try_new(stats_file).expect("builder for stats");
        let meta = builder.metadata();
        assert!(
            meta.num_row_groups() >= 1,
            "file must have at least one row group"
        );
        let rg = meta.row_group(0);
        // Column 0 = id (Int64) — stats must be present (min/max from DBP encoding).
        let col_stats = rg.column(0).statistics();
        assert!(
            col_stats.is_some(),
            "statistics should be present for the id column (Int64)"
        );
        // Column 1 = category (Utf8) — stats must be present.
        let cat_stats = rg.column(1).statistics();
        assert!(
            cat_stats.is_some(),
            "statistics should be present for the category column (Utf8)"
        );
    }

    // ── Data round-trip ──────────────────────────────────────────────────────
    // Read back via file-based reader.
    let loaded = ColumnarTable::read_from(&path).expect("read from file");
    let _ = std::fs::remove_file(&path);

    assert_eq!(loaded.row_count(), 1_000, "row count after file round-trip");

    // Spot-check: first 5 ids.
    let ids = loaded.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("downcast id");
    for i in 0..5_usize {
        assert_eq!(ids.value(i), i as i64, "id[{i}] mismatch");
    }

    // Spot-check: first 5 categories cycle correctly.
    let cats = loaded.batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("downcast category");
    let expected_cats = ["alpha", "beta", "gamma", "delta", "epsilon"];
    for i in 0..5 {
        assert_eq!(
            cats.value(i),
            expected_cats[i % expected_cats.len()],
            "category[{i}] mismatch"
        );
    }
}

#[test]
fn dictionary_and_stats_round_trip_via_bytes() {
    let table = make_large_compressible_table();
    let bytes = table.write_to_bytes().expect("write_to_bytes");
    let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

    assert_eq!(
        loaded.row_count(),
        1_000,
        "row count after bytes round-trip"
    );
    assert_eq!(loaded.schema.fields().len(), 3, "column count mismatch");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 4: projection still works after compression (bytes path)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "compress")]
fn projection_after_compression_bytes() {
    let table = make_large_compressible_table().with_compression(6);
    let bytes = table.write_to_bytes().expect("write_to_bytes compressed");

    // Decompress and load.
    let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes");

    // Project to only "id" and "score" columns.
    let projected = loaded.project(&["id", "score"]).expect("project");
    assert_eq!(projected.schema.fields().len(), 2, "projected column count");
    assert_eq!(projected.row_count(), 1_000, "projected row count");

    let proj_ids = projected.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("downcast id");
    for i in 0..5_usize {
        assert_eq!(proj_ids.value(i), i as i64, "projected id[{i}] mismatch");
    }
}

#[test]
#[cfg(feature = "compress")]
fn projection_after_compression_file() {
    let path = std::env::temp_dir().join(format!(
        "oxistore-columnar-proj-compress-{}.parquet",
        std::process::id()
    ));

    // Write compressed table to file (file-based writes use raw parquet, no OxiARC envelope).
    let table = make_large_compressible_table().with_compression(6);
    table.write_to(&path).expect("write_to file");

    // Read with projection (column 0 = id only).
    let batches = oxistore_columnar::read_batches_with_projection(&path, &[0])
        .expect("read_batches_with_projection");
    let _ = std::fs::remove_file(&path);

    assert!(!batches.is_empty(), "expected at least one batch");
    assert_eq!(batches[0].num_columns(), 1, "projected column count");
    assert_eq!(batches[0].num_rows(), 1_000, "projected row count");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 5: backward compat — uncompressed payload readable with compress feature
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "compress")]
fn uncompressed_payload_readable_with_compress_feature() {
    // Write WITHOUT compression.
    let table = make_small_table();
    let bytes = table.write_to_bytes().expect("write_to_bytes uncompressed");

    // Must start with Parquet magic PAR1, not OXIA.
    assert_ne!(&bytes[..4], b"OXIA", "no OXIA magic for uncompressed");

    // Read back transparently — should still work.
    let loaded = ColumnarTable::read_from_bytes(&bytes)
        .expect("read_from_bytes (uncompressed, compress feature enabled)");
    assert_eq!(
        loaded.row_count(),
        3,
        "row count after uncompressed re-read"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 6: compression level 0 (store) round-trip
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "compress")]
fn compression_level_zero_round_trip() {
    let table = make_small_table().with_compression(0);
    let bytes = table.write_to_bytes().expect("write_to_bytes level=0");

    assert_eq!(&bytes[..4], b"OXIA", "OXIA magic present even at level 0");

    let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read_from_bytes level=0");
    assert_eq!(loaded.row_count(), 3, "row count after level=0 round-trip");

    let names = loaded.batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("downcast name");
    assert_eq!(names.value(0), "alice");
    assert_eq!(names.value(1), "bob");
    assert_eq!(names.value(2), "carol");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 7: CompressionMode::None produces raw Parquet (not gated on feature)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn compression_mode_default_is_none() {
    let table = make_small_table();
    assert_eq!(
        table.compression,
        CompressionMode::None,
        "default compression mode should be None"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 8: with_compression clamps level to 9
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn with_compression_clamps_level() {
    let table = make_small_table().with_compression(255);
    assert_eq!(
        table.compression,
        CompressionMode::OxiArc { level: 9 },
        "level should be clamped to 9"
    );
}

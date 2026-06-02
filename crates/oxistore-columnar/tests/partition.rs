//! Integration tests for `PartitionedDataset` — Hive-style partitioned datasets.

use std::sync::Arc;

use oxistore_columnar::{
    Array, DataType, Field, Int64Array, PartitionPredicate, PartitionedDataset, RecordBatch,
    Schema, StringArray,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a temporary directory path unique to the calling test (by pid + name).
fn tmp_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxistore_partition_test_{}_{}_{}",
        name,
        std::process::id(),
        // extra randomness to avoid cross-test collisions when run in parallel
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0),
    ))
}

/// Build a batch with columns `[id: Int64, region: Utf8]`.
///
/// `rows` is a slice of `(id, region)` pairs.
fn make_batch(rows: &[(i64, &str)]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, false),
    ]));
    let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();
    let regions: Vec<&str> = rows.iter().map(|(_, r)| *r).collect();
    RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(regions)),
        ],
    )
    .expect("valid batch")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Writing two partitions creates the expected directory structure and files.
#[test]
fn partition_write_creates_directories() {
    let dir = tmp_dir("write_creates_dirs");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    let batch = make_batch(&[(1, "east"), (2, "west"), (3, "east")]);
    ds.write_partitioned(&[batch]).expect("write_partitioned");

    // manifest.tsv must exist
    assert!(
        dir.join("manifest.tsv").exists(),
        "manifest.tsv must be created"
    );
    // Both partition directories must exist
    assert!(
        dir.join("region=east").exists(),
        "region=east directory must be created"
    );
    assert!(
        dir.join("region=west").exists(),
        "region=west directory must be created"
    );
    // Each partition directory must contain part-0000.parquet
    assert!(
        dir.join("region=east/part-0000.parquet").exists(),
        "part-0000.parquet must exist under region=east"
    );
    assert!(
        dir.join("region=west/part-0000.parquet").exists(),
        "part-0000.parquet must exist under region=west"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Write-then-read round-trip preserves all rows across all partitions.
#[test]
fn partition_round_trip() {
    let dir = tmp_dir("round_trip");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    let batch = make_batch(&[
        (1, "north"),
        (2, "south"),
        (3, "north"),
        (4, "east"),
        (5, "south"),
    ]);
    ds.write_partitioned(&[batch]).expect("write");

    let batches = ds.read_partitioned(None).expect("read");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 5, "all 5 rows must survive the round-trip");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `PartitionPredicate::Eq` returns only the rows from the matching partition.
#[test]
fn partition_pruning_eq() {
    let dir = tmp_dir("pruning_eq");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    let batch = make_batch(&[
        (1, "alpha"),
        (2, "beta"),
        (3, "gamma"),
        (4, "alpha"),
        (5, "beta"),
    ]);
    ds.write_partitioned(&[batch]).expect("write");

    let pred = PartitionPredicate::Eq("alpha".to_string());
    let batches = ds
        .read_partitioned(Some(&pred))
        .expect("read with predicate");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2, "only 2 rows belong to partition 'alpha'");

    // Verify all returned rows have region == "alpha".
    for batch in &batches {
        let schema = batch.schema();
        let region_idx = schema.index_of("region").expect("region column");
        let region_col = batch
            .column(region_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("StringArray");
        for i in 0..region_col.len() {
            assert_eq!(
                region_col.value(i),
                "alpha",
                "all returned rows must be from partition 'alpha'"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// `PartitionPredicate::In([])` returns zero rows (empty match set).
#[test]
fn partition_empty_predicate() {
    let dir = tmp_dir("empty_pred");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    let batch = make_batch(&[(1, "x"), (2, "y"), (3, "z")]);
    ds.write_partitioned(&[batch]).expect("write");

    let pred = PartitionPredicate::In(vec![]);
    let batches = ds
        .read_partitioned(Some(&pred))
        .expect("read with empty In predicate");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total_rows, 0,
        "empty In([]) predicate must produce zero rows"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Partition values containing spaces and Unicode are encoded safely as
/// directory names (the original value is still round-tripped via manifest).
#[test]
fn partition_special_chars_in_value() {
    let dir = tmp_dir("special_chars");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    // Value with a space and an emoji — unsafe for raw directory names.
    let special_value = "us west \u{1F30D}";
    let batch = make_batch(&[(1, special_value), (2, "plain")]);
    ds.write_partitioned(&[batch]).expect("write");

    // The manifest must list the original value verbatim.
    let partitions = ds.list_partitions().expect("list_partitions");
    // partition_values is Vec<String>; for single-column it has exactly one element.
    let values: Vec<&str> = partitions
        .iter()
        .filter_map(|(vs, _, _)| vs.first().map(String::as_str))
        .collect();
    assert!(
        values.contains(&special_value),
        "manifest must preserve the original (un-encoded) partition value"
    );

    // Round-trip: reading with Eq on the original value must work.
    let pred = PartitionPredicate::Eq(special_value.to_string());
    let batches = ds
        .read_partitioned(Some(&pred))
        .expect("read special partition");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total_rows, 1,
        "exactly 1 row belongs to the special-value partition"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Three-partition write; `PartitionPredicate::In` selects two of them.
#[test]
fn partition_pruning_in_two_of_three() {
    let dir = tmp_dir("pruning_in");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    let batch = make_batch(&[(1, "a"), (2, "b"), (3, "c"), (4, "a"), (5, "c")]);
    ds.write_partitioned(&[batch]).expect("write");

    let pred = PartitionPredicate::In(vec!["a".to_string(), "c".to_string()]);
    let batches = ds.read_partitioned(Some(&pred)).expect("read In predicate");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    // "a" has rows 1,4 → 2 rows; "c" has rows 3,5 → 2 rows; total = 4
    assert_eq!(
        total_rows, 4,
        "In([a,c]) must return 4 rows (partitions a and c)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `PartitionPredicate::Range` selects partitions lexicographically within bounds.
#[test]
fn partition_pruning_range() {
    let dir = tmp_dir("pruning_range");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    // Partitions: "aaa", "bbb", "ccc", "ddd"
    let batch = make_batch(&[(1, "aaa"), (2, "bbb"), (3, "ccc"), (4, "ddd")]);
    ds.write_partitioned(&[batch]).expect("write");

    // Range ["bbb", "ddd") → "bbb" and "ccc" match; "aaa" and "ddd" are excluded.
    let pred = PartitionPredicate::Range {
        lo: "bbb".to_string(),
        hi: "ddd".to_string(),
    };
    let batches = ds
        .read_partitioned(Some(&pred))
        .expect("read Range predicate");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total_rows, 2,
        "Range [bbb, ddd) must return 2 rows (bbb and ccc)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `list_partitions` returns one entry per unique partition value with the
/// correct row count.
#[test]
fn partition_list_partitions() {
    let dir = tmp_dir("list_partitions");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    let batch = make_batch(&[(1, "x"), (2, "y"), (3, "x"), (4, "x")]);
    ds.write_partitioned(&[batch]).expect("write");

    let mut partitions = ds.list_partitions().expect("list_partitions");
    // Sort by the first partition value so assertion order is deterministic.
    partitions.sort_by(|a, b| {
        a.0.first()
            .map(String::as_str)
            .unwrap_or("")
            .cmp(b.0.first().map(String::as_str).unwrap_or(""))
    });

    assert_eq!(partitions.len(), 2);
    // For single-column partitions, the inner Vec has exactly one element.
    assert_eq!(partitions[0].0.first().map(String::as_str), Some("x"));
    assert_eq!(partitions[0].2, 3, "partition 'x' must have 3 rows");
    assert_eq!(partitions[1].0.first().map(String::as_str), Some("y"));
    assert_eq!(partitions[1].2, 1, "partition 'y' must have 1 row");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Writing multiple batches that share partition values merges them correctly.
#[test]
fn partition_multi_batch_write() {
    let dir = tmp_dir("multi_batch");
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");

    let batch1 = make_batch(&[(1, "east"), (2, "west")]);
    let batch2 = make_batch(&[(3, "east"), (4, "east")]);
    ds.write_partitioned(&[batch1, batch2]).expect("write");

    let pred = PartitionPredicate::Eq("east".to_string());
    let batches = ds
        .read_partitioned(Some(&pred))
        .expect("read east partition");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 3, "rows 1, 3, 4 all belong to partition 'east'");

    let _ = std::fs::remove_dir_all(&dir);
}

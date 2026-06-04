//! Criterion benchmarks for `oxistore-columnar` operations.
//!
//! Currently benchmarks:
//! - `write_partitioned_1k` — write 1 000 rows split across two string partitions.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use oxistore_columnar::{
    DataType, Field, Float64Array, Int64Array, PartitionedDataset, RecordBatch, Schema, StringArray,
};

/// Build a record batch with `n` rows and two partitions (`"east"` / `"west"`).
fn make_batch(n: usize) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, false),
    ]));
    let ids: Vec<i64> = (0..n as i64).collect();
    let regions: Vec<&str> = (0..n)
        .map(|i| if i % 2 == 0 { "east" } else { "west" })
        .collect();
    RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(regions)),
        ],
    )
    .expect("valid batch")
}

/// Produce a unique-enough suffix for temporary directory names using the
/// current time's subsecond nanoseconds combined with the process id.
fn rand_suffix() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (std::process::id() as u64)
}

fn bench_write_partitioned(c: &mut Criterion) {
    let mut group = c.benchmark_group("columnar");
    group.throughput(Throughput::Elements(1_000));
    group.bench_function("write_partitioned_1k", |b| {
        b.iter(|| {
            let dir = std::env::temp_dir().join(format!("oxistore_col_bench_{}", rand_suffix()));
            let ds = PartitionedDataset::new_single_column(dir.clone(), "region");
            let batch = make_batch(1_000);
            ds.write_partitioned(&[batch]).expect("write_partitioned");
            let _ = std::fs::remove_dir_all(&dir);
        });
    });
    group.finish();
}

fn bench_read_partitioned(c: &mut Criterion) {
    // Set up a dataset once outside the timed loop.
    let dir = std::env::temp_dir().join(format!("oxistore_col_bench_read_{}", rand_suffix()));
    let ds = PartitionedDataset::new_single_column(dir.clone(), "region");
    let batch = make_batch(1_000);
    ds.write_partitioned(&[batch])
        .expect("write for read bench");

    let mut group = c.benchmark_group("columnar");
    group.throughput(Throughput::Elements(1_000));
    group.bench_function("read_partitioned_1k", |b| {
        b.iter(|| {
            let batches = ds.read_partitioned(None).expect("read_partitioned");
            let _total: usize = batches.iter().map(|b| b.num_rows()).sum();
        });
    });
    group.finish();

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Column pruning benchmark
// ---------------------------------------------------------------------------

/// Build a Parquet byte payload with `n_cols` Int64 columns and `n_rows` rows.
fn make_wide_parquet(n_cols: usize, n_rows: usize) -> Vec<u8> {
    use oxistore_columnar::{ColumnarTable, DataType, Field};

    let fields: Vec<Field> = (0..n_cols)
        .map(|c| Field::new(format!("col_{c}"), DataType::Int64, false))
        .collect();
    let schema = Arc::new(Schema::new(fields));

    let vals: Vec<i64> = (0..n_rows as i64).collect();
    let columns: Vec<Arc<dyn arrow::array::Array>> = (0..n_cols)
        .map(|_| Arc::new(Int64Array::from(vals.clone())) as Arc<dyn arrow::array::Array>)
        .collect();

    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).expect("wide batch");

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch).expect("push");
    table.write_to_bytes().expect("write_to_bytes")
}

fn bench_column_pruning(c: &mut Criterion) {
    const N_COLS: usize = 20;
    const N_ROWS: usize = 1_000;

    // Prepare once outside the timed loop.
    let bytes = make_wide_parquet(N_COLS, N_ROWS);
    let all_cols: Vec<&str> = (0..N_COLS)
        .map(|c| Box::leak(format!("col_{c}").into_boxed_str()) as &str)
        .collect();
    let two_cols = &["col_0", "col_1"];

    let mut group = c.benchmark_group("column_pruning");
    group.throughput(Throughput::Elements(N_ROWS as u64));

    group.bench_function("read_all_20_cols", |b| {
        use oxistore_columnar::ColumnarTable;
        b.iter(|| {
            let loaded = ColumnarTable::read_columns(&bytes, &all_cols).expect("read all columns");
            black_box(loaded.row_count());
        });
    });

    group.bench_function("read_2_of_20_cols", |b| {
        use oxistore_columnar::ColumnarTable;
        b.iter(|| {
            let loaded = ColumnarTable::read_columns(&bytes, two_cols).expect("read 2 columns");
            black_box(loaded.row_count());
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Predicate pushdown benchmark
// ---------------------------------------------------------------------------

/// Build Parquet with 10 row groups of 100 rows each (x: 0..1000).
fn make_multigroup_parquet() -> Vec<u8> {
    use oxistore_columnar::{ColumnarTable, DataType, Field, WriterConfig};

    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
    let config = WriterConfig {
        max_row_group_size: Some(100),
    };

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    for group in 0..10 {
        let base = group * 100i64;
        let vals: Vec<i64> = (base..base + 100).collect();
        let batch =
            RecordBatch::try_new(Arc::clone(&schema), vec![Arc::new(Int64Array::from(vals))])
                .expect("batch");
        table.push(batch).expect("push");
    }
    table.write_to_bytes_with_config(&config).expect("write")
}

fn bench_predicate_pushdown(c: &mut Criterion) {
    use oxistore_columnar::{CmpOp, ColumnarTable, Predicate, Scalar};

    let bytes = make_multigroup_parquet();

    // Predicate that keeps all rows.
    let pred_all = Predicate::All;
    // Predicate that keeps only the last row group (900..1000 > 850).
    let pred_prune = Predicate::Cmp {
        column: "x".to_string(),
        op: CmpOp::Gt,
        value: Scalar::Int64(850),
    };

    let mut group = c.benchmark_group("predicate_pushdown");
    group.throughput(Throughput::Elements(1_000));

    group.bench_function("scan_all_groups", |b| {
        b.iter(|| {
            let result = ColumnarTable::read_with_predicate(&bytes, &pred_all)
                .expect("read_with_predicate all");
            black_box(result.row_count());
        });
    });

    group.bench_function("prune_most_groups", |b| {
        b.iter(|| {
            let result = ColumnarTable::read_with_predicate(&bytes, &pred_prune)
                .expect("read_with_predicate prune");
            black_box(result.row_count());
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Encoding round-trip benchmark for different batch sizes
// ---------------------------------------------------------------------------

fn bench_encoding_roundtrip(c: &mut Criterion) {
    use oxistore_columnar::{ColumnarTable, DataType, Field};

    const SMALL: usize = 1_000;
    const MEDIUM: usize = 10_000;
    const LARGE: usize = 100_000;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("val", DataType::Float64, false),
        Field::new("tag", DataType::Utf8, false),
    ]));

    let make_table = |n: usize| {
        let ids: Vec<i64> = (0..n as i64).collect();
        let vals: Vec<f64> = ids.iter().map(|&v| v as f64 * 0.001).collect();
        let tags: Vec<&str> = ids
            .iter()
            .map(|i| if i % 2 == 0 { "even" } else { "odd" })
            .collect();

        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(ids)) as Arc<dyn arrow::array::Array>,
                Arc::new(Float64Array::from(vals)),
                Arc::new(StringArray::from(tags)),
            ],
        )
        .expect("batch");

        let mut table = ColumnarTable::new(Arc::clone(&schema));
        table.push(batch).expect("push");
        table
    };

    let mut group = c.benchmark_group("encoding_roundtrip");

    for &n in &[SMALL, MEDIUM, LARGE] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            criterion::BenchmarkId::new("write_to_bytes", n),
            &n,
            |b, &n| {
                let table = make_table(n);
                b.iter(|| {
                    let bytes = table.write_to_bytes().expect("write_to_bytes");
                    black_box(bytes.len());
                });
            },
        );
        group.bench_with_input(
            criterion::BenchmarkId::new("write_read_roundtrip", n),
            &n,
            |b, &n| {
                let table = make_table(n);
                b.iter(|| {
                    let bytes = table.write_to_bytes().expect("write");
                    let loaded = ColumnarTable::read_from_bytes(&bytes).expect("read");
                    black_box(loaded.row_count());
                });
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Streaming writer vs batch writer throughput
// ---------------------------------------------------------------------------

fn bench_streaming_vs_batch_write(c: &mut Criterion) {
    use oxistore_columnar::{ColumnarStreamWriter, ColumnarTable, DataType, Field};

    const N_ROWS: usize = 10_000;
    const BATCH_SIZE: usize = 1_000;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("val", DataType::Float64, false),
        Field::new("tag", DataType::Utf8, false),
    ]));

    let make_batch = |offset: usize| {
        let ids: Vec<i64> = (offset as i64..(offset + BATCH_SIZE) as i64).collect();
        let vals: Vec<f64> = ids.iter().map(|&v| v as f64 * 0.001).collect();
        let tags: Vec<&str> = ids
            .iter()
            .map(|i| if i % 2 == 0 { "even" } else { "odd" })
            .collect();
        RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(ids)) as Arc<dyn arrow::array::Array>,
                Arc::new(Float64Array::from(vals)),
                Arc::new(StringArray::from(tags)),
            ],
        )
        .expect("batch")
    };

    let batches: Vec<RecordBatch> = (0..N_ROWS / BATCH_SIZE)
        .map(|b| make_batch(b * BATCH_SIZE))
        .collect();

    let mut group = c.benchmark_group("streaming_vs_batch");
    group.throughput(Throughput::Elements(N_ROWS as u64));

    group.bench_function("batch_writer", |b| {
        b.iter(|| {
            let mut table = ColumnarTable::new(Arc::clone(&schema));
            for batch in &batches {
                table.push(batch.clone()).expect("push");
            }
            let bytes = table.write_to_bytes().expect("write_to_bytes");
            black_box(bytes.len());
        });
    });

    group.bench_function("streaming_writer", |b| {
        b.iter(|| {
            let mut buf = Vec::new();
            let mut writer =
                ColumnarStreamWriter::new(Arc::clone(&schema), &mut buf, None).expect("stream");
            for batch in &batches {
                writer.write_batch(batch).expect("write_batch");
            }
            writer.finish().expect("finish");
            black_box(buf.len());
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_write_partitioned,
    bench_read_partitioned,
    bench_column_pruning,
    bench_predicate_pushdown,
    bench_encoding_roundtrip,
    bench_streaming_vs_batch_write,
);
criterion_main!(benches);

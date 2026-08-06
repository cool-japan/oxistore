//! Parquet write/read with column projection and predicate (row-group)
//! pushdown.
//!
//! ```sh
//! cargo run -p oxistore-columnar --example parquet_pushdown
//! ```
//!
//! Builds a 1 000-row table split into 10 row groups (100 rows each), then
//! shows three read paths side by side: full read, column-projected read
//! (only the requested columns are decoded), and predicate-pushed-down read
//! (row groups that provably cannot match are skipped using Parquet's
//! min/max statistics, without decoding their data).

use std::sync::Arc;

use oxistore_columnar::{
    CmpOp, ColumnarTable, DataType, Field, Float64Array, Int64Array, Predicate, RecordBatch,
    Scalar, Schema, StringArray, WriterConfig,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Build a 1 000-row table: id (Int64), region (Utf8), amount (Float64) ─
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("amount", DataType::Float64, false),
    ]));

    const N: usize = 1_000;
    let ids: Vec<i64> = (0..N as i64).collect();
    let regions: Vec<&str> = (0..N)
        .map(|i| if i % 2 == 0 { "east" } else { "west" })
        .collect();
    let amounts: Vec<f64> = ids.iter().map(|&id| id as f64 * 1.5).collect();

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(ids)),
            Arc::new(StringArray::from(regions)),
            Arc::new(Float64Array::from(amounts)),
        ],
    )?;

    let mut table = ColumnarTable::new(Arc::clone(&schema));
    table.push(batch)?;

    // 100 rows per row group -> 10 row groups, needed for predicate pushdown
    // to have something to prune.
    let config = WriterConfig {
        max_row_group_size: Some(100),
    };
    let bytes = table.write_to_bytes_with_config(&config)?;
    println!("wrote {} bytes for {N} rows", bytes.len());

    // ── Metadata-only read: row-group count without decoding any rows ────
    let meta = oxistore_columnar::read_metadata_from_bytes(&bytes)?;
    println!(
        "metadata: num_rows={}, num_row_groups={}, num_columns={}",
        meta.num_rows, meta.num_row_groups, meta.num_columns
    );

    // ── Full read: all columns, all row groups ────────────────────────────
    let full = ColumnarTable::read_from_bytes(&bytes)?;
    println!(
        "full read -> {} rows, {} columns",
        full.row_count(),
        full.schema.fields().len()
    );

    // ── Column projection: only decode the columns we asked for ──────────
    let projected = ColumnarTable::read_columns(&bytes, &["id", "amount"])?;
    println!(
        "read_columns([id, amount]) -> {} rows, {} columns: {:?}",
        projected.row_count(),
        projected.schema.fields().len(),
        projected
            .schema
            .fields()
            .iter()
            .map(|f| f.name().as_str())
            .collect::<Vec<_>>()
    );

    // ── Predicate pushdown: prune row groups using min/max statistics ────
    // Row groups are 100 rows each with monotonically increasing `id`
    // (group 0 = ids 0..100, group 8 = ids 800..900, group 9 = 900..1000, …).
    // Pruning happens at *row-group* granularity, using interval arithmetic
    // over each group's min/max statistics — a group is skipped only when
    // it is provably impossible for any row inside it to match; a group
    // that is kept is decoded in full (no row is filtered out of a kept
    // group). `id > 850`: group 8's max is 899 (> 850, so "maybe") and
    // group 9's max is 999 (> 850, so "maybe") — both kept in full (200
    // rows: the whole 800..1000 range). Groups 0..8 all have max <= 799,
    // provably cannot satisfy `> 850`, and are skipped without decoding.
    let prune_most = Predicate::Cmp {
        column: "id".to_string(),
        op: CmpOp::Gt,
        value: Scalar::Int64(850),
    };
    let pruned = ColumnarTable::read_with_predicate(&bytes, &prune_most)?;
    println!(
        "read_with_predicate(id > 850) -> {} rows (row groups 8 & 9 kept whole, groups 0-7 skipped)",
        pruned.row_count()
    );

    // `id < 0`: every group's min is >= 0, so every group is provably
    // impossible to match and all 10 are skipped -> 0 rows decoded at all.
    let prune_none = Predicate::Cmp {
        column: "id".to_string(),
        op: CmpOp::Lt,
        value: Scalar::Int64(0),
    };
    let none = ColumnarTable::read_with_predicate(&bytes, &prune_none)?;
    println!(
        "read_with_predicate(id < 0)   -> {} rows (all 10 row groups skipped)",
        none.row_count()
    );

    // ── Combined projection + predicate pushdown ──────────────────────────
    let combined =
        ColumnarTable::read_with_projection_and_predicate(&bytes, &["region"], &prune_most)?;
    println!(
        "read_with_projection_and_predicate([region], id > 850) -> {} rows, columns={:?}",
        combined.row_count(),
        combined
            .schema
            .fields()
            .iter()
            .map(|f| f.name().as_str())
            .collect::<Vec<_>>()
    );

    Ok(())
}

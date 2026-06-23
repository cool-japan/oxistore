# oxistore-columnar — Pure-Rust Parquet columnar storage for OxiStore

[![Crates.io](https://img.shields.io/crates/v/oxistore-columnar.svg)](https://crates.io/crates/oxistore-columnar)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-columnar` is the columnar storage format of the OxiStore stack. It is a thin, ergonomic layer over Apache **Arrow** and **Parquet** that persists `RecordBatch`es as Parquet files and in-memory buffers, with first-class support for projection pushdown, row-group predicate pruning, Hive-style partitioned datasets, and streaming read/write.

Files written by this crate use **UNCOMPRESSED** parquet-internal encoding (dictionary encoding, delta encodings, and page-level statistics are always enabled). The `parquet` dependency is compiled with `default-features = false` so that **no** `snap`, `brotli`, `flate2`, `lz4`, `zstd`, or `miniz_oxide` codec is ever linked. When optional payload compression is wanted, it is applied as an outer OxiARC DEFLATE envelope rather than a Parquet-internal codec — keeping the crate consistent with the COOLJAPAN Pure-Rust compression policy. The crate is **`#![forbid(unsafe_code)]`**.

## Installation

```toml
[dependencies]
oxistore-columnar = "0.2.0"
```

With OxiARC payload compression enabled:

```toml
[dependencies]
oxistore-columnar = { version = "0.2.0", features = ["compress"] }
```

## Quick Start

```rust,no_run
use std::sync::Arc;
use oxistore_columnar::{ColumnarTable, Schema, Field, DataType, Int64Array, RecordBatch};

# fn main() -> Result<(), oxistore_columnar::ColumnarError> {
let schema = Arc::new(Schema::new(vec![
    Field::new("id", DataType::Int64, false),
]));

let mut table = ColumnarTable::new(Arc::clone(&schema));
let batch = RecordBatch::try_new(
    Arc::clone(&schema),
    vec![Arc::new(Int64Array::from(vec![1, 2, 3]))],
)?;
table.push(batch)?;

// Round-trip to a Parquet file
table.write_to(std::path::Path::new("/tmp/my.parquet"))?;
let reloaded = ColumnarTable::read_from(std::path::Path::new("/tmp/my.parquet"))?;
assert_eq!(reloaded.row_count(), 3);
# Ok(())
# }
```

### Predicate pruning + projection

```rust,no_run
use oxistore_columnar::{ColumnarTable, Predicate, CmpOp, Scalar};

# fn demo(bytes: &[u8]) -> Result<(), oxistore_columnar::ColumnarError> {
let pred = Predicate::Cmp {
    column: "id".into(),
    op: CmpOp::Ge,
    value: Scalar::Int64(2),
};

// Read only the `id` column, skipping row groups that cannot match the predicate.
let table = ColumnarTable::read_with_projection_and_predicate(bytes, &["id"], &pred)?;
# Ok(())
# }
```

### OxiARC payload compression (feature `compress`)

```rust,no_run
# use std::sync::Arc;
# use oxistore_columnar::{ColumnarTable, Schema, Field, DataType};
# let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
let table = ColumnarTable::new(schema).with_compression(6); // level 0..=9
// write_to_bytes() now prefixes the Parquet payload with the 4-byte b"OXIA" magic
// header and OxiARC DEFLATE-compressed bytes; read_from_bytes() inflates automatically.
```

## API Overview

### `ColumnarTable`

The primary high-level type — a shared [`Schema`] plus an ordered `Vec<RecordBatch>` and a [`CompressionMode`].

| Method | Description |
|--------|-------------|
| `new(schema)` | Empty table, `CompressionMode::None` |
| `with_compression(level)` | Enable OxiARC DEFLATE for `*_bytes` round-trips (level clamped 0–9) |
| `push(batch)` / `push_unchecked(batch)` | Append a batch (schema-checked / unchecked) |
| `row_count()` | Total rows across all batches |
| `merge(&other)` | Append another table's batches (schemas must match) |
| `project(&["col", …])` | New table with only the named columns |
| `sort_by(column, ascending)` | New table sorted by a column (concatenates into one batch) |
| `filter(&predicate)` | New table keeping only rows where `predicate` holds (row-level eval) |
| `write_to(path)` / `read_from(path)` | File round-trip (UNCOMPRESSED parquet codec) |
| `write_to_bytes()` / `read_from_bytes(bytes)` | In-memory round-trip (honours `CompressionMode`) |
| `write_to_with_config(path, &cfg)` / `write_to_bytes_with_config(&cfg)` | Round-trip with a custom [`WriterConfig`] |
| `read_columns(bytes, &["col", …])` | Projection pushdown via Parquet `ProjectionMask` |
| `read_with_predicate(bytes, &pred)` | Row-group pruning from min/max/null statistics |
| `read_with_projection_and_predicate(bytes, &cols, &pred)` | Projection + pruning together |
| `read_with_schema(bytes, &target)` | Schema adaptation: missing columns null-filled, extra ignored, type clash errors |
| `metadata_from_bytes(bytes)` | Footer-only metadata → [`ParquetFileMetaInfo`] |

`ColumnarTable` also implements `Display` and the [`ColumnarStore`] trait.

### `ColumnarStore` trait

Abstracts over Arrow-`RecordBatch`-backed stores; implemented by `ColumnarTable`.
Methods: `schema`, `batches`, `compression`, `row_count`, `push`, `push_unchecked`,
`project`, `sort_by`, `filter`, `write_to_bytes`, `write_to`,
`write_to_bytes_with_config`, `write_to_with_config`.

### `ColumnarTableBuilder`

| Method | Description |
|--------|-------------|
| `new(schema)` | Start a builder with default options |
| `.row_group_size(n)` | Advisory max rows per row group |
| `.compression(level)` | Enable OxiARC DEFLATE (0–9) |
| `.row_group_size_hint()` | Read back the configured hint |
| `.build()` | Build an empty `ColumnarTable` |
| `.build_with_config()` | Build a `(ColumnarTable, WriterConfig)` pair honouring the row-group hint |

### Free functions

| Function | Description |
|----------|-------------|
| `write_batches(path, schema, &batches)` | Write batches to a Parquet file |
| `read_batches(path)` | Read all batches from a file |
| `read_batches_with_projection(path, &indices)` | Read only the listed column indices |
| `read_metadata(path)` | Footer metadata → [`ParquetFileMetadata`] |
| `read_metadata_from_bytes(bytes)` | Footer metadata from bytes → [`ParquetFileMetaInfo`] (handles the `OXIA` header) |
| `read_with_predicate(path, &pred)` | File read with row-group pruning |
| `read_with_projection_and_predicate(path, &cols, &pred)` | File read with projection + pruning |

### Predicate engine (`predicate` module)

| Type | Description |
|------|-------------|
| `Predicate` | AST: `Cmp { column, op, value }`, `And(Vec)`, `Or(Vec)`, `Not(Box)`, `All`, `None` |
| `Predicate::evaluate_batch(&batch)` | Row-level evaluation → `BooleanArray` mask |
| `Predicate::row_group_might_match(&rg, &schema)` | Conservative row-group statistics pruning |
| `CmpOp` | `Eq`, `Ne`, `Lt`, `Le`, `Gt`, `Ge` |
| `Scalar` | `Bool`, `Int32`, `Int64`, `Float32`, `Float64`, `Bytes(Vec<u8>)`, `Null` |

`Not`, missing statistics, and type mismatches are handled conservatively — the row group is always kept.

### Partitioned datasets (`partition` module)

| Type | Description |
|------|-------------|
| `PartitionedDataset` | Hive-style multi-file dataset (`<root>/<col>=<val>/…/part-NNNN.parquet`) with a `manifest.tsv` (v1 single-column / v2 multi-column, auto-detected). Methods: `new`, `new_single_column`, `with_compression`, `root`, `partition_columns`, `write_partitioned`, `read_partitioned`, `list_partitions` |
| `PartitionPredicate` | Pruning predicate: `Eq(String)`, `In(Vec<String>)`, `Range { lo, hi }`, `And(Vec<(String, PartitionPredicate)>)` |

### Streaming (`streaming` module)

| Type | Description |
|------|-------------|
| `ColumnarStreamWriter<W: Write + Send>` | Incremental Parquet writer. Methods: `new(schema, sink, props)`, `write_batch(&batch)`, `finish()` |
| `ColumnarStreamReader` | Lazy in-memory Parquet reader; `Iterator<Item = Result<RecordBatch, ColumnarError>>`. Methods: `from_bytes(bytes)`, `schema()` |

### `WriterConfig`

| Field | Description |
|-------|-------------|
| `max_row_group_size: Option<usize>` | Max rows per row group; `None` uses the parquet default. Smaller values enable finer-grained predicate pushdown |

### Metadata types

| Type | Fields |
|------|--------|
| `ParquetFileMetadata` | `schema`, `num_rows`, `num_row_groups`, `num_columns`, `file_size` |
| `ParquetFileMetaInfo` | `num_rows`, `num_row_groups`, `num_columns`, `file_size` (no schema; footer-only) |

### `CompressionMode`

| Variant | Description |
|---------|-------------|
| `None` (default) | Raw Parquet bytes — backward compatible |
| `OxiArc { level }` | Outer OxiARC DEFLATE envelope (level 0–9). Requires the `compress` feature; the level is clamped to 0–9 |

### Re-exported Arrow types

For convenience the crate re-exports the Arrow surface needed to build and inspect batches: `Array`, `ArrayRef`, `BooleanArray`, `Float32Array`, `Float64Array`, `Int32Array`, `Int64Array`, `LargeStringArray`, `StringArray`, `UInt32Array`, `UInt64Array`, `compute`, `DataType`, `Field`, `Schema`, and `RecordBatch`.

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `columnar` | off | Marker feature (no extra deps); present for facade symmetry |
| `compress` | off | Pulls in `oxiarc-deflate` and enables `CompressionMode::OxiArc` payload compression. **Never** links flate2, zstd, brotli, snap, or lz4 |

## Error variants

`ColumnarError` implements `std::error::Error` and `Display`, with `From` conversions from `std::io::Error`, `arrow::error::ArrowError`, and `parquet::errors::ParquetError`.

| Variant | Description |
|---------|-------------|
| `Io(std::io::Error)` | Filesystem I/O error |
| `Arrow(ArrowError)` | Arrow-level error (schema mismatch, allocation, …) |
| `Parquet(ParquetError)` | Parquet serialisation/deserialisation error |
| `SchemaMismatch(String)` | Batch/table schema mismatch |
| `Compress(String)` | OxiARC payload compression/decompression failure |
| `UnsupportedType(String)` | Arrow type not serialisable to Parquet (currently `DataType::Union`) |
| `Manifest(String)` | Partition manifest missing, malformed, or unsupported version |

## OxiARC compression backend

When the `compress` feature is enabled, compression is provided exclusively by **`oxiarc-deflate`** (Pure-Rust DEFLATE, RFC 1951) from the COOLJAPAN OxiARC stack — `deflate(data, level)` on write and `inflate(data)` on read. Compressed `*_bytes` output is prefixed with the 4-byte magic header `b"OXIA"`; uncompressed payloads (no header) remain fully readable, so the format is backward-compatible.

## Cross-references

- [`oxistore`](https://crates.io/crates/oxistore) — the storage facade; enable the `columnar` feature to re-export this crate.
- [`oxistore-compress`](https://crates.io/crates/oxistore-compress) — the standalone OxiARC codec bridge (also includes a Parquet `Codec` shim).

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

# oxistore-columnar TODO

## Status
Basic Parquet read/write implemented via Apache Arrow. `ColumnarTable` provides in-memory batch storage with `write_to` and `read_from` for Parquet files. All files use UNCOMPRESSED encoding (compression deferred to oxistore-compress/OxiARC). Re-exports common Arrow types. ~171 SLOC across 3 files (lib.rs, reader.rs, writer.rs).

## Core Implementation
- [x] Add column pruning (projection pushdown) to `read_batches` — `ColumnarTable::read_columns(bytes, columns)` using `ProjectionMask::leaves`; skip I/O for unneeded columns (~60 SLOC) (done 2026-05-25)
- [x] Add predicate pushdown to `read_batches` — full predicate AST (`Predicate`, `CmpOp`, `Scalar`) in `src/predicate.rs`; `ColumnarTable::read_with_predicate` skips row groups via Parquet min/max/null_count statistics (~409 SLOC) (done 2026-05-25)
- [x] Add row group filtering — `read_with_predicate` examines `RowGroupMetaData` statistics and calls `builder.with_row_groups(surviving)` to skip provably non-matching groups (~40 SLOC) (done 2026-05-25)
- [x] Add row group size configuration to `write_batches` — `WriterConfig { max_row_group_size }` passed to `set_max_row_group_row_count`; exposed via `write_to_bytes_with_config` and `write_to_with_config` (~50 SLOC) (done 2026-05-25)
- [x] Add dictionary encoding support — enable dictionary encoding globally in `WriterProperties` with `DELTA_BINARY_PACKED` / `DELTA_LENGTH_BYTE_ARRAY` per-column fallback (~45 SLOC) (done 2026-05-25)
- [x] Add run-length encoding (RLE) support — `RLE_DICTIONARY` enabled automatically via dictionary flag in `WriterProperties` (~5 SLOC) (done 2026-05-25)
- [x] Add delta encoding support — `DELTA_BINARY_PACKED` for integer columns, `DELTA_LENGTH_BYTE_ARRAY` for string/binary in `WriterProperties` (~35 SLOC) (done 2026-05-25)
- [x] Add page-level statistics — `EnabledStatistics::Page` enabled globally via `WriterProperties` (~5 SLOC) (done 2026-05-25)
- [x] Add Parquet metadata introspection — `read_metadata(path)` returning schema, row count, row group count, file size without reading data (~25 SLOC) (done 2026-05-25; also added read_metadata_from_bytes for in-memory buffers)
- [x] Add streaming writer — `ColumnarStreamWriter<W: Write+Send>` in `src/streaming.rs` wrapping `ArrowWriter`; incremental `write_batch` + `finish` (~80 SLOC) (done 2026-05-25)
- [x] Add streaming reader — `ColumnarStreamReader` in `src/streaming.rs` wrapping `ParquetRecordBatchReader`; implements `Iterator<Item=Result<RecordBatch, ColumnarError>>` (~50 SLOC) (done 2026-05-25)
- [x] Add schema evolution support — `ColumnarTable::read_with_schema(bytes, target)`: projects common columns, fills missing target columns with null arrays, rejects type mismatches with `SchemaMismatch` (~80 SLOC) (done 2026-05-25)
- [x] Add multi-file table support — Hive-style multi-column partitioned datasets with v2 manifest (`src/partition.rs`) (done 2026-05-27)
- [x] Add OxiARC compression integration — payload-level OxiARC DEFLATE envelope for `write_to_bytes`/`read_from_bytes`; `OXIA` magic header for auto-detection; `CompressionMode` enum; `with_compression(level)` builder (done 2026-05-25)
- [x] Add `ColumnarTable::merge(other)` — merge two tables with the same schema (~20 SLOC) (done 2026-05-25)
- [x] Add `ColumnarTable::filter(predicate)` — return a new table with rows matching a predicate (~30 SLOC) (done 2026-05-25)
- [x] Add `ColumnarTable::project(columns)` — return a new table with selected columns only (~20 SLOC) (done 2026-05-25)
- [x] Add `ColumnarTable::sort_by(column, ascending)` — sort batches by a column (~30 SLOC) (done 2026-05-25)
- [x] Add `ColumnarTable::row_count()` — total rows across all batches (~10 SLOC) (done 2026-05-25)

## API Improvements
- [x] Add `ColumnarError::SchemaMismatch` variant for schema validation failures (~10 SLOC) (done 2026-05-25)
- [x] Add schema validation on `ColumnarTable::push` — reject batches that don't match the table schema (~15 SLOC) (done 2026-05-25)
- [x] Add `write_to_bytes` / `read_from_bytes` for in-memory Parquet serialization without filesystem (~25 SLOC) (done 2026-05-25)
- [x] Re-export additional Arrow types needed by downstream crates (BooleanArray, UInt64Array, etc.) (~10 SLOC) (done 2026-05-27)
- [x] Add `ColumnarTableBuilder` with options for row group size, encoding, and compression strategy (~30 SLOC) (done 2026-05-25)
- [x] Implement `Display` for `ColumnarTable` showing schema and row count summary (~10 SLOC) (done 2026-05-25)

## Testing
- [x] Round-trip test with all supported Arrow data types — integer (i8/i16/i32/i64/u8/u16/u32/u64), float (f32/f64), string (Utf8/LargeUtf8), binary (Binary/LargeBinary), Boolean (done 2026-06-03)
- [x] Test reading Parquet files with missing columns (schema evolution) — `schema_evolution_missing_column_filled_with_null` (done 2026-06-03)
- [x] Test writing and reading large tables (1M+ rows) to verify streaming correctness — `large_table_1m_rows_streaming_correctness` via ColumnarStreamWriter (done 2026-06-03)
- [x] Test row group boundary alignment — `row_group_boundary_alignment_integrity`: writes 250 rows (150+100 batches) with max_row_group_size=100, verifies 3 row groups produced and all values 0..249 are contiguous across boundaries (~55 SLOC) (done 2026-06-03)
- [x] Test `ColumnarTable::push` with schema-mismatched batches — `push_schema_mismatch_rejected` (done 2026-06-03)
- [x] Test reading externally-generated Parquet files (from pyarrow, pandas) — `tests/external_parquet.rs`: 6 tests covering PLAIN-encoded files, nullable columns, multi-row-group files, column projection, schema evolution null-fill, and predicate pushdown on externally-written files (done 2026-06-03)
- [x] Benchmark read/write throughput with varying column counts and row counts — `bench_encoding_roundtrip` covers Small/Medium/Large × write/roundtrip in `columnar_ops.rs` (done 2026-06-03)
- [x] Test dictionary-encoded columns round-trip correctly — `dictionary_encoded_utf8_round_trip` with 1000 rows × 10 categories (done 2026-06-03)
- [x] Property-based test: write random batches, read back, verify equality — full proptest suite in `proptest_roundtrip.rs`: Int32/Int64/UInt32/UInt64/Float32/Float64/String/multi-column (done 2026-06-03)

## Performance
- [x] Benchmark column pruning speedup — `bench_column_pruning` in `columnar_ops.rs`: reads 2 vs all 20 columns from a 1k-row/20-col Parquet payload (done 2026-06-03)
- [x] Benchmark predicate pushdown — `bench_predicate_pushdown` in `columnar_ops.rs`: scans all groups vs prunes most groups on a 10-group dataset (done 2026-06-03)
- [x] Benchmark encoding impact — `bench_encoding_roundtrip` in `columnar_ops.rs`: write_to_bytes and full roundtrip at Small/Medium/Large sizes (done 2026-06-03)
- [x] Profile memory usage during large file reads — verify streaming reader avoids full materialization — `tests/streaming_memory.rs`: 5 tests covering lazy iteration, incremental sum vs bulk, bounded batch sizes (8192-row chunks from 100k-row groups), partial iteration safety, and schema access before iteration (done 2026-06-03)
- [x] Benchmark write throughput with streaming writer vs batch writer — `bench_streaming_vs_batch_write` in `columnar_ops.rs`: 10k rows × 10 batches, batch vs streaming writer (done 2026-06-03)

## Integration
- [x] Implement `ColumnarStore` trait for `ColumnarTable` — full trait in oxistore-columnar with all delegation methods (~145 SLOC) (done 2026-05-27)
- [x] Integration with `oxisql-datafusion` — `ParquetTableProvider` in `oxisql-datafusion/src/parquet.rs` uses `oxistore_columnar::read_metadata`, `read_batches`, and `read_batches_with_projection`; enabled via `columnar` feature flag in oxisql-datafusion (done in oxisql-datafusion crate)
- [x] Integration with `oxistore-blob` — `oxistore-blob/tests/columnar_integration.rs` demonstrates full round-trip: `ColumnarTable::write_to_bytes` → `BlobStore::put` → `BlobStore::get` → `ColumnarTable::read_from_bytes`; CAS deduplication and list-prefix tests included (done in oxistore-blob crate)
- [x] Integration with `oxistore-cache` — `ColumnarRowGroupCache` in `oxistore-cache/src/columnar_cache.rs` caches serialised Parquet row groups with LRU eviction, TTL support, hit/miss tracking, and file-level invalidation; enabled via `columnar` feature flag (done in oxistore-cache crate)

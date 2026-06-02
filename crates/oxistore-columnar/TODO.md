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
- [ ] Round-trip test with all supported Arrow data types (Boolean, Int8-64, UInt8-64, Float32/64, Utf8, LargeUtf8, Binary, LargeBinary) (~40 SLOC)
- [ ] Test reading Parquet files with missing columns (schema evolution) (~25 SLOC)
- [ ] Test writing and reading large tables (1M+ rows) to verify streaming correctness (~20 SLOC)
- [ ] Test row group boundary alignment — verify data integrity when batch sizes don't align with row group sizes (~20 SLOC)
- [ ] Test `ColumnarTable::push` with schema-mismatched batches (~15 SLOC)
- [ ] Test reading externally-generated Parquet files (from pyarrow, pandas) (~20 SLOC)
- [ ] Benchmark read/write throughput with varying column counts and row counts (~40 SLOC)
- [ ] Test dictionary-encoded columns round-trip correctly (~20 SLOC)
- [ ] Property-based test: write random batches, read back, verify equality (~30 SLOC)

## Performance
- [ ] Benchmark column pruning speedup — measure I/O saved when reading 2 of 100 columns (~30 SLOC)
- [ ] Benchmark predicate pushdown — measure row groups skipped with selective filters (~30 SLOC)
- [ ] Benchmark encoding impact — dictionary vs plain vs RLE on representative datasets (~40 SLOC)
- [ ] Profile memory usage during large file reads — verify streaming reader avoids full materialization (~25 SLOC)
- [ ] Benchmark write throughput with streaming writer vs batch writer (~30 SLOC)

## Integration
- [x] Implement `ColumnarStore` trait for `ColumnarTable` — full trait in oxistore-columnar with all delegation methods (~145 SLOC) (done 2026-05-27)
- [ ] Integration with `oxisql-datafusion` — serve `ColumnarTable` as a DataFusion `TableProvider` reading Parquet files from `oxistore-blob` storage (~40 SLOC)
- [ ] Integration with `oxistore-blob` — read/write Parquet files to/from blob storage backends (~30 SLOC)
- [ ] Integration with `oxistore-cache` — cache frequently accessed row groups in LRU/ARC cache (~35 SLOC)

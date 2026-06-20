#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore-columnar` — Parquet read/write via Apache Arrow `RecordBatch`.
//!
//! This crate provides a thin, ergonomic layer over the `arrow` and `parquet`
//! crates to persist [`RecordBatch`]es as Parquet files.  Files written by
//! this crate use **UNCOMPRESSED** parquet-internal encoding.  When the
//! `compress` feature is enabled, an OxiARC DEFLATE envelope can be applied
//! at the payload level via [`CompressionMode`].
//!
//! # Quick example
//!
//! ```no_run
//! use std::sync::Arc;
//! use oxistore_columnar::{ColumnarTable, Schema, Field, DataType};
//!
//! let schema = Arc::new(Schema::new(vec![
//!     Field::new("id", DataType::Int64, false),
//! ]));
//! let mut table = ColumnarTable::new(Arc::clone(&schema));
//! // ... push batches ...
//! # let path = std::env::temp_dir().join("my.parquet");
//! table.write_to(&path).expect("write failed");
//! ```
//!
//! # Encoding policy
//!
//! All Parquet files written by oxistore-columnar use:
//! - Dictionary encoding enabled globally (`RLE_DICTIONARY` for low-cardinality data).
//! - `DELTA_BINARY_PACKED` for integer columns (efficient for monotonic sequences).
//! - `DELTA_LENGTH_BYTE_ARRAY` for string/binary columns.
//! - Page-level statistics (`EnabledStatistics::Page`) for predicate pushdown.
//!
//! # OxiARC compression policy
//!
//! The `parquet` dependency is compiled with `default-features = false` and
//! only the `arrow` + `experimental` features enabled.  This means no snap,
//! brotli, flate2, lz4, zstd, or miniz_oxide codec is ever compiled into this
//! crate.
//!
//! When the `compress` feature is enabled and a `ColumnarTable` is built with
//! `with_compression(level)`, the `write_to_bytes()` output is prefixed with a
//! 4-byte magic header (`b"OXIA"`) followed by OxiARC DEFLATE-compressed
//! Parquet bytes.  `read_from_bytes()` detects the header and inflates
//! automatically — uncompressed payloads remain fully backward-compatible.

/// Predicate AST and row-group pruning engine.
///
/// Re-exports: [`Predicate`], [`CmpOp`], [`Scalar`].
pub mod predicate;
mod reader;
mod writer;

/// Hive-style partitioned dataset: multi-column write/read with manifest v1/v2.
///
/// Re-exports: [`PartitionedDataset`], [`PartitionPredicate`].
pub mod partition;

/// Streaming Parquet writer and reader.
///
/// Re-exports: [`ColumnarStreamWriter`], [`ColumnarStreamReader`].
pub mod streaming;

pub use arrow::array::{
    Array, ArrayRef, BooleanArray, Float32Array, Float64Array, Int32Array, Int64Array,
    LargeStringArray, StringArray, UInt32Array, UInt64Array,
};
pub use arrow::compute;
pub use arrow::datatypes::{DataType, Field, Schema};
pub use arrow::record_batch::RecordBatch;
pub use partition::{PartitionPredicate, PartitionedDataset};
pub use predicate::{CmpOp, Predicate, Scalar};
pub use streaming::{ColumnarStreamReader, ColumnarStreamWriter};
pub use writer::WriterConfig;

/// Trait that abstracts over columnar stores backed by Arrow [`RecordBatch`]es.
///
/// [`ColumnarTable`] implements this trait; other in-memory or on-disk
/// columnar containers may implement it to participate in the same ecosystem.
///
/// All methods are required; [`ColumnarTable`] provides the canonical
/// implementation, and other columnar containers may implement the trait
/// to participate in the same ecosystem.
pub trait ColumnarStore {
    /// Return a reference to the store's Arrow [`Schema`].
    fn schema(&self) -> &Arc<Schema>;

    /// Return a slice of all [`RecordBatch`]es held by the store.
    fn batches(&self) -> &[RecordBatch];

    /// Return the current [`CompressionMode`] of the store.
    fn compression(&self) -> CompressionMode;

    /// Return the total number of rows across all batches.
    fn row_count(&self) -> usize;

    /// Append a batch, validating that its schema matches the store's schema.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::SchemaMismatch`] if schemas differ.
    fn push(&mut self, batch: RecordBatch) -> Result<(), ColumnarError>;

    /// Append a batch without schema validation.
    fn push_unchecked(&mut self, batch: RecordBatch);

    /// Return a new store containing only the named columns.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on projection failure.
    fn project(&self, columns: &[&str]) -> Result<ColumnarTable, ColumnarError>;

    /// Return a new store with all rows sorted by `column_name`.
    ///
    /// `ascending = true` → ascending order; `false` → descending order.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] if the column does not exist.
    fn sort_by(&self, column_name: &str, ascending: bool) -> Result<ColumnarTable, ColumnarError>;

    /// Return a new store containing only rows where `predicate` is satisfied.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on evaluation failure.
    fn filter(&self, predicate: &Predicate) -> Result<ColumnarTable, ColumnarError>;

    /// Serialise the store to Parquet bytes (with optional OxiARC compression).
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on serialisation failure.
    fn write_to_bytes(&self) -> Result<Vec<u8>, ColumnarError>;

    /// Persist the store as a Parquet file at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on I/O or serialisation failure.
    fn write_to(&self, path: &std::path::Path) -> Result<(), ColumnarError>;

    /// Serialise the store to Parquet bytes using a custom [`WriterConfig`].
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on serialisation failure.
    fn write_to_bytes_with_config(&self, config: &WriterConfig) -> Result<Vec<u8>, ColumnarError>;

    /// Persist the store as a Parquet file at `path` using a custom [`WriterConfig`].
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on I/O or serialisation failure.
    fn write_to_with_config(
        &self,
        path: &std::path::Path,
        config: &WriterConfig,
    ) -> Result<(), ColumnarError>;
}

impl ColumnarStore for ColumnarTable {
    fn schema(&self) -> &Arc<Schema> {
        &self.schema
    }

    fn batches(&self) -> &[RecordBatch] {
        &self.batches
    }

    fn compression(&self) -> CompressionMode {
        self.compression
    }

    fn row_count(&self) -> usize {
        self.row_count()
    }

    fn push(&mut self, batch: RecordBatch) -> Result<(), ColumnarError> {
        self.push(batch)
    }

    fn push_unchecked(&mut self, batch: RecordBatch) {
        self.push_unchecked(batch);
    }

    fn project(&self, columns: &[&str]) -> Result<ColumnarTable, ColumnarError> {
        self.project(columns)
    }

    fn sort_by(&self, column_name: &str, ascending: bool) -> Result<ColumnarTable, ColumnarError> {
        self.sort_by(column_name, ascending)
    }

    fn filter(&self, predicate: &Predicate) -> Result<ColumnarTable, ColumnarError> {
        self.filter(predicate)
    }

    fn write_to_bytes(&self) -> Result<Vec<u8>, ColumnarError> {
        self.write_to_bytes()
    }

    fn write_to(&self, path: &std::path::Path) -> Result<(), ColumnarError> {
        self.write_to(path)
    }

    fn write_to_bytes_with_config(&self, config: &WriterConfig) -> Result<Vec<u8>, ColumnarError> {
        self.write_to_bytes_with_config(config)
    }

    fn write_to_with_config(
        &self,
        path: &std::path::Path,
        config: &WriterConfig,
    ) -> Result<(), ColumnarError> {
        self.write_to_with_config(path, config)
    }
}

/// Lightweight metadata extracted from a Parquet byte buffer without reading
/// any row data.
///
/// Returned by [`read_metadata_from_bytes`] and
/// [`ColumnarTable::metadata_from_bytes`].
#[derive(Debug, Clone)]
pub struct ParquetFileMetaInfo {
    /// Total number of rows across all row groups.
    pub num_rows: i64,
    /// Number of row groups in the Parquet file.
    pub num_row_groups: usize,
    /// Number of columns (Arrow schema fields).
    pub num_columns: usize,
    /// Size of the input byte slice in bytes.
    pub file_size: u64,
}

/// Read Parquet metadata from an in-memory byte slice without reading any row data.
///
/// Handles both raw Parquet bytes and OxiARC-compressed payloads (with the
/// 4-byte `b"OXIA"` magic header).
///
/// # Errors
///
/// Returns [`ColumnarError`] on Parquet parse or decompression failure.
pub fn read_metadata_from_bytes(data: &[u8]) -> Result<ParquetFileMetaInfo, ColumnarError> {
    reader::read_metadata_from_bytes(data)
}

use std::path::Path;
use std::sync::Arc;

/// 4-byte magic header that prefixes OxiARC-compressed Parquet payloads.
///
/// The presence of this header distinguishes compressed from uncompressed
/// `write_to_bytes` output and lets `read_from_bytes` transparently inflate.
const OXIA_MAGIC: &[u8; 4] = b"OXIA";

/// Error type for columnar I/O operations.
#[derive(Debug)]
pub enum ColumnarError {
    /// A filesystem I/O error.
    Io(std::io::Error),
    /// An Arrow-level error (schema mismatch, buffer allocation, ...).
    Arrow(arrow::error::ArrowError),
    /// A Parquet-level serialisation or deserialisation error.
    Parquet(parquet::errors::ParquetError),
    /// Schema mismatch between batches or table and incoming data.
    SchemaMismatch(String),
    /// Compression or decompression error (OxiARC payload).
    Compress(String),
    /// The schema contains an Arrow type not supported for Parquet serialisation.
    ///
    /// Currently the only unsupported type is `DataType::Union` (at any nesting
    /// depth).  The inner `String` names the offending type.
    UnsupportedType(String),
    /// A partition manifest is missing, malformed, or has an unsupported version.
    Manifest(String),
}

impl std::fmt::Display for ColumnarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ColumnarError::Io(e) => write!(f, "columnar I/O error: {e}"),
            ColumnarError::Arrow(e) => write!(f, "Arrow error: {e}"),
            ColumnarError::Parquet(e) => write!(f, "Parquet error: {e}"),
            ColumnarError::SchemaMismatch(msg) => write!(f, "schema mismatch: {msg}"),
            ColumnarError::Compress(msg) => write!(f, "compression error: {msg}"),
            ColumnarError::UnsupportedType(msg) => write!(f, "unsupported Arrow type: {msg}"),
            ColumnarError::Manifest(msg) => write!(f, "partition manifest error: {msg}"),
        }
    }
}

impl std::error::Error for ColumnarError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ColumnarError::Io(e) => Some(e),
            ColumnarError::Arrow(e) => Some(e),
            ColumnarError::Parquet(e) => Some(e),
            ColumnarError::SchemaMismatch(_)
            | ColumnarError::Compress(_)
            | ColumnarError::UnsupportedType(_)
            | ColumnarError::Manifest(_) => None,
        }
    }
}

impl From<std::io::Error> for ColumnarError {
    fn from(e: std::io::Error) -> Self {
        ColumnarError::Io(e)
    }
}

impl From<arrow::error::ArrowError> for ColumnarError {
    fn from(e: arrow::error::ArrowError) -> Self {
        ColumnarError::Arrow(e)
    }
}

impl From<parquet::errors::ParquetError> for ColumnarError {
    fn from(e: parquet::errors::ParquetError) -> Self {
        ColumnarError::Parquet(e)
    }
}

/// Controls whether `write_to_bytes` / `read_from_bytes` apply OxiARC DEFLATE
/// compression to the serialised Parquet payload.
///
/// # Variants
///
/// - [`None`](CompressionMode::None) — no outer compression; the bytes are raw
///   Parquet (default, backward-compatible).
/// - [`OxiArc`](CompressionMode::OxiArc) — the payload is wrapped in an OxiARC
///   DEFLATE envelope with the given compression level (0 = store, 9 = best
///   compression).  Requires the `compress` crate feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionMode {
    /// No outer compression — raw Parquet bytes.
    #[default]
    None,
    /// OxiARC DEFLATE envelope.
    ///
    /// `level` is clamped to 0–9.  Requires the `compress` feature.
    OxiArc {
        /// Compression level, 0 (store) to 9 (best compression).
        level: u8,
    },
}

/// Write `batches` to a Parquet file at `path`.
///
/// All batches must share `schema`.  The output file will use UNCOMPRESSED
/// parquet-internal encoding with dictionary encoding and page-level statistics
/// enabled.
///
/// # Errors
///
/// Propagates [`ColumnarError`] on any I/O or Arrow/Parquet failure.
pub fn write_batches(
    path: &Path,
    schema: Arc<Schema>,
    batches: &[RecordBatch],
) -> Result<(), ColumnarError> {
    writer::write_batches(path, schema, batches)
}

/// Read all [`RecordBatch`]es from the Parquet file at `path`.
///
/// # Errors
///
/// Propagates [`ColumnarError`] on I/O or deserialisation failures.
pub fn read_batches(path: &Path) -> Result<Vec<RecordBatch>, ColumnarError> {
    reader::read_batches(path)
}

/// Serialise `batches` to an in-memory Parquet byte buffer.
///
/// This is the in-memory counterpart of [`write_batches`].  All batches must
/// share `schema`.  The output uses UNCOMPRESSED parquet-internal encoding with
/// dictionary encoding and page-level statistics.
///
/// For OxiARC DEFLATE payload compression, build a [`ColumnarTable`] and call
/// [`ColumnarTable::write_to_bytes`] after enabling `with_compression`.
///
/// # Errors
///
/// Propagates [`ColumnarError`] on serialisation failure.
pub fn write_batches_to_bytes(
    schema: Arc<Schema>,
    batches: &[RecordBatch],
) -> Result<Vec<u8>, ColumnarError> {
    writer::write_batches_to_bytes(schema, batches)
}

/// Read all [`RecordBatch`]es from an in-memory Parquet byte buffer.
///
/// This is the in-memory counterpart of [`read_batches`].
///
/// # Errors
///
/// Propagates [`ColumnarError`] on Parquet deserialisation failure.
pub fn read_batches_from_bytes(data: &[u8]) -> Result<Vec<RecordBatch>, ColumnarError> {
    reader::read_batches_from_bytes(data).map(|t| t.batches)
}

/// Read all [`RecordBatch`]es from the Parquet file at `path`, selecting
/// only the specified columns (projection pushdown / column pruning).
///
/// `column_indices` specifies which columns to read (zero-based indices
/// into the file schema).  Columns not listed are skipped entirely,
/// saving I/O.
///
/// # Errors
///
/// Propagates [`ColumnarError`] on I/O or deserialisation failures.
pub fn read_batches_with_projection(
    path: &Path,
    column_indices: &[usize],
) -> Result<Vec<RecordBatch>, ColumnarError> {
    reader::read_batches_with_projection(path, column_indices)
}

/// Read Parquet file metadata without reading any row data.
///
/// Returns a summary of the file's schema, row count, row groups, and size.
pub fn read_metadata(path: &Path) -> Result<ParquetFileMetadata, ColumnarError> {
    reader::read_metadata(path)
}

/// Read a Parquet file from `path` applying row-group predicate pruning.
///
/// Row groups whose statistics prove they cannot match `predicate` are skipped.
/// Returns the surviving rows as a `Vec<RecordBatch>`.
///
/// # Errors
///
/// Returns [`ColumnarError`] on I/O or Parquet parse failure.
pub fn read_with_predicate(
    path: &Path,
    predicate: &predicate::Predicate,
) -> Result<Vec<RecordBatch>, ColumnarError> {
    let bytes = std::fs::read(path)?;
    let table = reader::read_with_predicate_from_bytes(&bytes, predicate)?;
    Ok(table.batches)
}

/// Read a Parquet file from `path` applying both column projection and
/// row-group predicate pruning in a single pass.
///
/// Only the columns named in `projection` are decoded; row groups that cannot
/// match `predicate` are skipped entirely.
///
/// # Errors
///
/// Returns [`ColumnarError`] on I/O, projection, or Parquet parse failure.
pub fn read_with_projection_and_predicate(
    path: &Path,
    projection: &[&str],
    predicate: &predicate::Predicate,
) -> Result<Vec<RecordBatch>, ColumnarError> {
    let bytes = std::fs::read(path)?;
    let table =
        reader::read_with_projection_and_predicate_from_bytes(&bytes, projection, predicate)?;
    Ok(table.batches)
}

/// Summary metadata from a Parquet file.
#[derive(Debug, Clone)]
pub struct ParquetFileMetadata {
    /// The Arrow schema inferred from the Parquet file.
    pub schema: Arc<Schema>,
    /// Total number of rows across all row groups.
    pub num_rows: i64,
    /// Number of row groups in the file.
    pub num_row_groups: usize,
    /// Number of columns.
    pub num_columns: usize,
    /// File size in bytes.
    pub file_size: u64,
}

/// An in-memory columnar table backed by a schema and a list of batches.
///
/// `ColumnarTable` is the primary high-level type for working with Parquet
/// files through oxistore-columnar.  It couples a shared [`Schema`] with an
/// ordered list of [`RecordBatch`]es and provides round-trip persistence.
///
/// # Compression
///
/// By default `write_to_bytes` produces raw Parquet bytes
/// (`CompressionMode::None`).  Call `with_compression(level)` to enable
/// OxiARC DEFLATE payload compression (requires the `compress` feature):
///
/// ```rust,no_run
/// # use std::sync::Arc;
/// # use oxistore_columnar::{ColumnarTable, Schema, Field, DataType};
/// # let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
/// let table = ColumnarTable::new(schema).with_compression(6);
/// ```
pub struct ColumnarTable {
    /// The Arrow schema shared by all batches in this table.
    pub schema: Arc<Schema>,
    /// The row batches stored in insertion order.
    pub batches: Vec<RecordBatch>,
    /// Compression mode applied to `write_to_bytes` output.
    pub compression: CompressionMode,
}

impl ColumnarTable {
    /// Create an empty table with the given schema and no outer compression.
    #[must_use]
    pub fn new(schema: Arc<Schema>) -> Self {
        ColumnarTable {
            schema,
            batches: Vec::new(),
            compression: CompressionMode::None,
        }
    }

    /// Return a new table with OxiARC DEFLATE compression enabled at the given
    /// level (0 = store, 9 = best).
    ///
    /// This affects only `write_to_bytes` / `read_from_bytes`; file-based
    /// writes (`write_to`) are not affected.
    ///
    /// Requires the `compress` feature to produce compressed output.
    /// On non-compress builds this is accepted but has no effect.
    #[must_use]
    pub fn with_compression(mut self, level: u8) -> Self {
        self.compression = CompressionMode::OxiArc {
            level: level.min(9),
        };
        self
    }

    /// Append a batch to the table.
    ///
    /// Returns `Err(ColumnarError::SchemaMismatch)` if the batch schema
    /// does not match the table schema.
    pub fn push(&mut self, batch: RecordBatch) -> Result<(), ColumnarError> {
        if batch.schema() != self.schema {
            return Err(ColumnarError::SchemaMismatch(format!(
                "expected schema {:?}, got {:?}",
                self.schema,
                batch.schema()
            )));
        }
        self.batches.push(batch);
        Ok(())
    }

    /// Append a batch without schema validation.
    ///
    /// Use this only when you have already verified the schema externally.
    pub fn push_unchecked(&mut self, batch: RecordBatch) {
        self.batches.push(batch);
    }

    /// Return the total number of rows across all batches.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.batches.iter().map(|b| b.num_rows()).sum()
    }

    /// Persist the table as a Parquet file at `path` (UNCOMPRESSED parquet codec).
    ///
    /// Dictionary encoding and page-level statistics are still written.
    ///
    /// # Errors
    ///
    /// Propagates [`ColumnarError`] on any I/O or serialisation failure.
    pub fn write_to(&self, path: &Path) -> Result<(), ColumnarError> {
        writer::write_batches(path, Arc::clone(&self.schema), &self.batches)
    }

    /// Reconstruct a `ColumnarTable` by reading all batches from `path`.
    ///
    /// # Errors
    ///
    /// Propagates [`ColumnarError`] on I/O or deserialisation failures.
    pub fn read_from(path: &Path) -> Result<Self, ColumnarError> {
        let batches = reader::read_batches(path)?;
        let schema = if let Some(first) = batches.first() {
            Arc::clone(first.schema_ref())
        } else {
            Arc::new(Schema::empty())
        };
        Ok(ColumnarTable {
            schema,
            batches,
            compression: CompressionMode::None,
        })
    }

    /// Return a new table containing only the specified columns.
    ///
    /// `columns` is a list of column names to retain.  Missing column
    /// names are silently ignored.
    pub fn project(&self, columns: &[&str]) -> Result<Self, ColumnarError> {
        // Build the new schema with only selected fields.
        let indices: Vec<usize> = columns
            .iter()
            .filter_map(|name| self.schema.index_of(name).ok())
            .collect();

        if indices.is_empty() {
            return Ok(ColumnarTable::new(Arc::new(Schema::empty())));
        }

        let new_fields: Vec<Arc<Field>> = indices
            .iter()
            .map(|&i| Arc::new(self.schema.field(i).clone()))
            .collect();
        let new_schema = Arc::new(Schema::new(new_fields));

        let mut new_batches = Vec::with_capacity(self.batches.len());
        for batch in &self.batches {
            let cols: Vec<ArrayRef> = indices
                .iter()
                .map(|&i| Arc::clone(batch.column(i)))
                .collect();
            let projected = RecordBatch::try_new(Arc::clone(&new_schema), cols)?;
            new_batches.push(projected);
        }

        Ok(ColumnarTable {
            schema: new_schema,
            batches: new_batches,
            compression: self.compression,
        })
    }

    /// Merge another table into this one.
    ///
    /// Both tables must have the same schema.  The other table's batches
    /// are appended after this table's batches.
    pub fn merge(&mut self, other: &ColumnarTable) -> Result<(), ColumnarError> {
        if self.schema != other.schema {
            return Err(ColumnarError::SchemaMismatch(format!(
                "cannot merge: schemas differ ({:?} vs {:?})",
                self.schema, other.schema
            )));
        }
        self.batches.extend(other.batches.iter().cloned());
        Ok(())
    }

    /// Sort all batches by the given column in ascending or descending order.
    ///
    /// This concatenates all batches into a single sorted batch.
    pub fn sort_by(&self, column_name: &str, ascending: bool) -> Result<Self, ColumnarError> {
        let idx = self
            .schema
            .index_of(column_name)
            .map_err(ColumnarError::Arrow)?;

        // Concatenate all batches.
        let combined = arrow::compute::concat_batches(&self.schema, &self.batches)?;

        // Sort by the specified column.
        let sort_column = combined.column(idx);
        let sort_options = arrow::compute::SortOptions {
            descending: !ascending,
            nulls_first: true,
        };
        let sort_indices = arrow::compute::sort_to_indices(sort_column, Some(sort_options), None)?;

        // Reorder all columns.
        let sorted_cols: Vec<ArrayRef> = combined
            .columns()
            .iter()
            .map(|col| arrow::compute::take(col.as_ref(), &sort_indices, None))
            .collect::<Result<Vec<_>, _>>()?;

        let sorted_batch = RecordBatch::try_new(Arc::clone(&self.schema), sorted_cols)?;

        Ok(ColumnarTable {
            schema: Arc::clone(&self.schema),
            batches: vec![sorted_batch],
            compression: self.compression,
        })
    }

    /// Write the table to an in-memory byte buffer (Parquet format).
    ///
    /// When `compression` is [`CompressionMode::OxiArc`] (and the `compress`
    /// feature is enabled), the output is prefixed with the 4-byte magic
    /// `b"OXIA"` followed by OxiARC DEFLATE-compressed Parquet bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on serialisation or compression failure.
    pub fn write_to_bytes(&self) -> Result<Vec<u8>, ColumnarError> {
        let raw = writer::write_batches_to_bytes(Arc::clone(&self.schema), &self.batches)?;

        match self.compression {
            CompressionMode::None => Ok(raw),
            CompressionMode::OxiArc { level } => compress_payload(&raw, level),
        }
    }

    /// Read a table from an in-memory byte buffer (Parquet format).
    ///
    /// If the buffer starts with the 4-byte magic `b"OXIA"`, the remaining
    /// bytes are treated as an OxiARC DEFLATE-compressed Parquet payload and
    /// inflated before parsing.  Uncompressed payloads (no magic header) are
    /// read directly.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on decompression or deserialisation failure.
    pub fn read_from_bytes(data: &[u8]) -> Result<Self, ColumnarError> {
        let parquet_bytes = if data.starts_with(OXIA_MAGIC) {
            decompress_payload(&data[OXIA_MAGIC.len()..])?
        } else {
            data.to_vec()
        };
        reader::read_batches_from_bytes(&parquet_bytes)
    }

    /// Read a table from bytes, projecting only the requested column names.
    ///
    /// Column names that do not exist in the file are silently ignored.
    /// Uses Parquet's `ProjectionMask` so unneeded column pages are not
    /// decoded or copied into memory.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on Parquet parse or schema failure.
    pub fn read_columns(bytes: &[u8], columns: &[&str]) -> Result<Self, ColumnarError> {
        reader::read_columns_from_bytes(bytes, columns)
    }

    /// Read a table from bytes, skipping row groups that cannot satisfy `pred`.
    ///
    /// The predicate is evaluated against Parquet row-group statistics
    /// (min/max/null-count).  Row groups that are provably unable to match
    /// are skipped entirely; the remaining row groups are fully decoded and
    /// returned.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on Parquet parse failure.
    pub fn read_with_predicate(
        bytes: &[u8],
        pred: &predicate::Predicate,
    ) -> Result<Self, ColumnarError> {
        reader::read_with_predicate_from_bytes(bytes, pred)
    }

    /// Read a table from bytes with both column projection and row-group
    /// predicate pruning applied together.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on Parquet parse or schema failure.
    pub fn read_with_projection_and_predicate(
        bytes: &[u8],
        columns: &[&str],
        pred: &predicate::Predicate,
    ) -> Result<Self, ColumnarError> {
        reader::read_with_projection_and_predicate_from_bytes(bytes, columns, pred)
    }

    /// Read a table from bytes, adapting the file schema to `target`.
    ///
    /// - Columns present in both file and target: read normally.
    /// - Columns in target but absent in file: filled with null arrays of the
    ///   target field's data type.
    /// - Columns in file but absent in target: ignored.
    /// - Columns present in both with mismatched types: returns
    ///   [`ColumnarError::SchemaMismatch`].
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::SchemaMismatch`] on type incompatibility, or
    /// [`ColumnarError::Parquet`] / [`ColumnarError::Arrow`] on I/O failure.
    pub fn read_with_schema(bytes: &[u8], target: &Arc<Schema>) -> Result<Self, ColumnarError> {
        reader::read_with_schema_from_bytes(bytes, target)
    }

    /// Write the table to bytes using a custom [`WriterConfig`].
    ///
    /// This is the configurable variant of [`Self::write_to_bytes`] — use it when
    /// you need to control the maximum row-group size or other writer options.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on serialisation failure.
    pub fn write_to_bytes_with_config(
        &self,
        config: &WriterConfig,
    ) -> Result<Vec<u8>, ColumnarError> {
        let raw = writer::write_batches_to_bytes_with_config(
            Arc::clone(&self.schema),
            &self.batches,
            config,
        )?;
        match self.compression {
            CompressionMode::None => Ok(raw),
            CompressionMode::OxiArc { level } => compress_payload(&raw, level),
        }
    }

    /// Persist the table as a Parquet file at `path` using a custom
    /// [`WriterConfig`] (e.g. to control the maximum row-group size).
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on I/O or serialisation failure.
    pub fn write_to_with_config(
        &self,
        path: &Path,
        config: &WriterConfig,
    ) -> Result<(), ColumnarError> {
        writer::write_batches_with_config(path, Arc::clone(&self.schema), &self.batches, config)
    }
}

/// Compress `raw` with OxiARC DEFLATE at `level` and prefix with `OXIA_MAGIC`.
///
/// Available only when the `compress` feature is enabled; on no-compress builds
/// the function body is a compile-time error (the call site is unreachable
/// because `CompressionMode::OxiArc` is still constructible but the feature
/// guard prevents accidental use in the no-compress path).
#[cfg(feature = "compress")]
fn compress_payload(raw: &[u8], level: u8) -> Result<Vec<u8>, ColumnarError> {
    let compressed =
        oxiarc_deflate::deflate(raw, level).map_err(|e| ColumnarError::Compress(e.to_string()))?;
    let mut out = Vec::with_capacity(OXIA_MAGIC.len() + compressed.len());
    out.extend_from_slice(OXIA_MAGIC);
    out.extend_from_slice(&compressed);
    Ok(out)
}

#[cfg(not(feature = "compress"))]
fn compress_payload(_raw: &[u8], _level: u8) -> Result<Vec<u8>, ColumnarError> {
    Err(ColumnarError::Compress(
        "OxiARC compression requires the `compress` feature".into(),
    ))
}

/// Inflate an OxiARC DEFLATE-compressed Parquet payload.
#[cfg(feature = "compress")]
fn decompress_payload(compressed: &[u8]) -> Result<Vec<u8>, ColumnarError> {
    oxiarc_deflate::inflate(compressed).map_err(|e| ColumnarError::Compress(e.to_string()))
}

#[cfg(not(feature = "compress"))]
fn decompress_payload(_compressed: &[u8]) -> Result<Vec<u8>, ColumnarError> {
    Err(ColumnarError::Compress(
        "OxiARC decompression requires the `compress` feature".into(),
    ))
}

impl ColumnarTable {
    /// Extract Parquet metadata from a byte buffer without reading row data.
    ///
    /// This is a convenience alias for the free function [`read_metadata_from_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] on Parquet parse or decompression failure.
    pub fn metadata_from_bytes(data: &[u8]) -> Result<ParquetFileMetaInfo, ColumnarError> {
        reader::read_metadata_from_bytes(data)
    }

    /// Filter this table's batches using `predicate`, returning a new table
    /// containing only the rows for which the predicate evaluates to `true`.
    ///
    /// Row-level evaluation is performed by [`Predicate::evaluate_batch`].
    /// Empty output batches (after filtering) are not retained.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] if the predicate references a column that does
    /// not exist or whose type is incompatible with the scalar in the predicate.
    pub fn filter(&self, predicate: &Predicate) -> Result<ColumnarTable, ColumnarError> {
        let mut filtered: Vec<RecordBatch> = Vec::with_capacity(self.batches.len());
        for batch in &self.batches {
            let mask = predicate.evaluate_batch(batch)?;
            let result =
                arrow::compute::filter_record_batch(batch, &mask).map_err(ColumnarError::Arrow)?;
            if result.num_rows() > 0 {
                filtered.push(result);
            }
        }
        Ok(ColumnarTable {
            schema: Arc::clone(&self.schema),
            batches: filtered,
            compression: self.compression,
        })
    }
}

// ── ColumnarTableBuilder ──────────────────────────────────────────────────────

/// Builder for constructing a [`ColumnarTable`] with optional configuration.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use oxistore_columnar::{ColumnarTableBuilder, Schema, Field, DataType};
///
/// let schema = Arc::new(Schema::new(vec![
///     Field::new("id", DataType::Int64, false),
/// ]));
/// let table = ColumnarTableBuilder::new(schema)
///     .row_group_size(1024)
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct ColumnarTableBuilder {
    schema: Arc<Schema>,
    /// Advisory maximum number of rows per row group.
    ///
    /// When `Some(n)`, the table stores this as metadata for downstream
    /// consumers.  Pass it to [`WriterConfig`] when serialising.
    row_group_size: Option<usize>,
    compression: CompressionMode,
}

impl ColumnarTableBuilder {
    /// Create a builder with the given schema and default options.
    #[must_use]
    pub fn new(schema: Arc<Schema>) -> Self {
        Self {
            schema,
            row_group_size: None,
            compression: CompressionMode::None,
        }
    }

    /// Set the advisory maximum number of rows per row group.
    ///
    /// The value is stored in the builder and exposed via
    /// [`ColumnarTableBuilder::row_group_size_hint`].  Pass it to
    /// [`WriterConfig`] when calling [`ColumnarTable::write_to_bytes_with_config`].
    #[must_use]
    pub fn row_group_size(mut self, size: usize) -> Self {
        self.row_group_size = Some(size);
        self
    }

    /// Enable OxiARC DEFLATE compression at the given level (0–9).
    #[must_use]
    pub fn compression(mut self, level: u8) -> Self {
        self.compression = CompressionMode::OxiArc {
            level: level.min(9),
        };
        self
    }

    /// Return the configured row-group size hint, if any.
    #[must_use]
    pub fn row_group_size_hint(&self) -> Option<usize> {
        self.row_group_size
    }

    /// Build the [`ColumnarTable`].
    ///
    /// The returned table is empty (no batches).  Populate it with
    /// [`ColumnarTable::push`] or [`ColumnarTable::push_unchecked`].
    #[must_use]
    pub fn build(self) -> ColumnarTable {
        ColumnarTable {
            schema: self.schema,
            batches: Vec::new(),
            compression: self.compression,
        }
    }

    /// Build the table and also return a [`WriterConfig`] configured from this
    /// builder's settings (e.g. `row_group_size`).
    ///
    /// Use with [`ColumnarTable::write_to_bytes_with_config`] or
    /// [`ColumnarTable::write_to_with_config`] to honour the row-group size hint.
    #[must_use]
    pub fn build_with_config(self) -> (ColumnarTable, WriterConfig) {
        let config = WriterConfig {
            max_row_group_size: self.row_group_size,
        };
        let table = ColumnarTable {
            schema: self.schema,
            batches: Vec::new(),
            compression: self.compression,
        };
        (table, config)
    }
}

impl std::fmt::Display for ColumnarTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ColumnarTable(schema={} cols, {} batches, {} rows)",
            self.schema.fields().len(),
            self.batches.len(),
            self.row_count()
        )
    }
}

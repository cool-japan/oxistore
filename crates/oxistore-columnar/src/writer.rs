/// Parquet writer using ArrowWriter with enhanced encoding and statistics.
///
/// Codec policy: all Parquet files written by oxistore-columnar use
/// `Compression::UNCOMPRESSED` at the parquet-internal layer.  Outer
/// OxiARC DEFLATE compression is applied at the payload level by
/// `ColumnarTable::write_to_bytes` / `read_from_bytes`.
///
/// Encoding policy (applied globally via WriterProperties):
/// - Dictionary encoding enabled for all columns — parquet will use
///   `RLE_DICTIONARY` automatically for low-cardinality data.
/// - `DELTA_BINARY_PACKED` fallback for integer columns (applied per-field
///   after dictionary overflow).
/// - `DELTA_LENGTH_BYTE_ARRAY` fallback for string/binary columns.
/// - Page-level statistics enabled (`EnabledStatistics::Page`) to support
///   predicate pushdown at the reader side.
use std::path::Path;
use std::sync::Arc;

use arrow::datatypes::{DataType, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_writer::ArrowWriter;
use parquet::basic::{Compression, Encoding};
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::schema::types::ColumnPath;

use crate::ColumnarError;

// ── Schema validation ─────────────────────────────────────────────────────────

/// Return `Err(ColumnarError::UnsupportedType)` if `dt` or any nested type
/// it contains is not supported for Parquet serialisation.
///
/// Currently the only unsupported type is `DataType::Union` (in any nesting
/// position).  List, Struct, Map, FixedSizeBinary, and Decimal128 are all
/// handled natively by the arrow-parquet writer.
fn check_datatype(dt: &DataType) -> Result<(), ColumnarError> {
    match dt {
        DataType::Union(_, _) => Err(ColumnarError::UnsupportedType(format!(
            "{:?} is not supported for Parquet serialisation",
            dt
        ))),
        DataType::List(inner)
        | DataType::LargeList(inner)
        | DataType::FixedSizeList(inner, _)
        | DataType::ListView(inner)
        | DataType::LargeListView(inner) => check_datatype(inner.data_type()),
        DataType::Map(inner, _) => check_datatype(inner.data_type()),
        DataType::Struct(fields) => {
            for f in fields {
                check_datatype(f.data_type())?;
            }
            Ok(())
        }
        DataType::Dictionary(_, value_type) => check_datatype(value_type),
        DataType::RunEndEncoded(_, values) => check_datatype(values.data_type()),
        _ => Ok(()),
    }
}

/// Validate that every field in `schema` uses a type supported for Parquet write.
///
/// # Errors
///
/// Returns [`ColumnarError::UnsupportedType`] if any column (or nested field)
/// has a type that cannot be serialised to Parquet by this crate.
pub(crate) fn validate_schema_for_write(schema: &Schema) -> Result<(), ColumnarError> {
    for field in schema.fields() {
        check_datatype(field.data_type())?;
    }
    Ok(())
}

/// Configuration for the Parquet writer.
///
/// Extends the base encoding/compression policy with optional parameters
/// that can be tuned per write call.
#[derive(Debug, Clone, Default)]
pub struct WriterConfig {
    /// Maximum number of rows per row group.
    ///
    /// When `None` the parquet crate default is used (128 MiB or
    /// approximately 1 million rows).  Setting a smaller value forces the
    /// writer to flush more row groups, which enables finer-grained
    /// predicate pushdown at read time.
    pub max_row_group_size: Option<usize>,
}

/// Build `WriterProperties` with enhanced encoding and page-level statistics.
///
/// - Dictionary encoding is enabled globally (parquet default is `true`, we
///   set it explicitly for clarity and to be robust against future default
///   changes).
/// - Parquet-level compression is always `UNCOMPRESSED`; OxiARC is applied at
///   the payload level by the caller.
/// - Page-level statistics (`EnabledStatistics::Page`) are written for all
///   columns so downstream readers can do predicate pushdown.
/// - Integer columns receive `DELTA_BINARY_PACKED` as their fallback encoding
///   (used after dictionary overflow or when the column is not dictionary-encoded).
/// - String / binary columns receive `DELTA_LENGTH_BYTE_ARRAY` as fallback.
pub(crate) fn build_writer_props(schema: &Schema) -> WriterProperties {
    build_writer_props_with_config(schema, &WriterConfig::default())
}

/// Build `WriterProperties` with enhanced encoding, page-level statistics,
/// and optional row-group-size configuration from [`WriterConfig`].
pub(crate) fn build_writer_props_with_config(
    schema: &Schema,
    config: &WriterConfig,
) -> WriterProperties {
    let mut builder = WriterProperties::builder()
        .set_compression(Compression::UNCOMPRESSED)
        .set_dictionary_enabled(true)
        .set_statistics_enabled(EnabledStatistics::Page);

    if config.max_row_group_size.is_some() {
        builder = builder.set_max_row_group_row_count(config.max_row_group_size);
    }

    // Apply per-column encoding hints based on Arrow data type.
    for field in schema.fields() {
        let col = ColumnPath::from(field.name().as_str());
        match field.data_type() {
            // Integer family — DELTA_BINARY_PACKED is efficient for sorted or
            // monotonically increasing integers (e.g. primary keys, timestamps).
            DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Date32
            | DataType::Date64
            | DataType::Time32(_)
            | DataType::Time64(_)
            | DataType::Timestamp(_, _)
            | DataType::Duration(_) => {
                builder = builder.set_column_encoding(col, Encoding::DELTA_BINARY_PACKED);
            }
            // String / binary family — DELTA_LENGTH_BYTE_ARRAY stores length
            // deltas compactly, good for variable-length columns.
            DataType::Utf8
            | DataType::LargeUtf8
            | DataType::Binary
            | DataType::LargeBinary
            | DataType::Utf8View
            | DataType::BinaryView => {
                builder = builder.set_column_encoding(col, Encoding::DELTA_LENGTH_BYTE_ARRAY);
            }
            // Boolean, float, and complex types — leave at parquet's default
            // (PLAIN or whatever the writer chooses with dictionary enabled).
            _ => {}
        }
    }

    builder.build()
}

/// Write `batches` to an in-memory buffer using the supplied [`WriterConfig`].
///
/// This is the configurable variant of [`write_batches_to_bytes`]; callers
/// that want to control row group size should use this function.
pub(crate) fn write_batches_to_bytes_with_config(
    schema: Arc<Schema>,
    batches: &[RecordBatch],
    config: &WriterConfig,
) -> Result<Vec<u8>, ColumnarError> {
    validate_schema_for_write(&schema)?;
    let props = build_writer_props_with_config(&schema, config);

    let mut buf = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props))?;

    for batch in batches {
        writer.write(batch)?;
    }

    writer.close()?;
    Ok(buf)
}

/// Write `batches` to a file at `path` using the supplied [`WriterConfig`].
pub(crate) fn write_batches_with_config(
    path: &Path,
    schema: Arc<Schema>,
    batches: &[RecordBatch],
    config: &WriterConfig,
) -> Result<(), ColumnarError> {
    validate_schema_for_write(&schema)?;
    let props = build_writer_props_with_config(&schema, config);
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;

    for batch in batches {
        writer.write(batch)?;
    }

    writer.close()?;
    Ok(())
}

/// Write `batches` to a Parquet file at `path` using UNCOMPRESSED encoding
/// with dictionary encoding and page-level statistics enabled.
///
/// All rows in every batch must conform to `schema`.  The file is created
/// (or truncated) at `path`; directories must already exist.
///
/// # Errors
///
/// Returns [`ColumnarError::Io`] on filesystem errors, [`ColumnarError::Arrow`]
/// on Arrow schema mismatches, and [`ColumnarError::Parquet`] on serialisation
/// failures.
pub(crate) fn write_batches(
    path: &Path,
    schema: Arc<Schema>,
    batches: &[RecordBatch],
) -> Result<(), ColumnarError> {
    validate_schema_for_write(&schema)?;
    let props = build_writer_props(&schema);
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;

    for batch in batches {
        writer.write(batch)?;
    }

    writer.close()?;
    Ok(())
}

/// Write `batches` to an in-memory byte buffer using UNCOMPRESSED parquet
/// encoding, with dictionary encoding and page-level statistics enabled.
///
/// Returns the raw serialised Parquet data as a `Vec<u8>`.
/// OxiARC compression (if desired) must be applied by the caller **after**
/// this function returns.
pub(crate) fn write_batches_to_bytes(
    schema: Arc<Schema>,
    batches: &[RecordBatch],
) -> Result<Vec<u8>, ColumnarError> {
    validate_schema_for_write(&schema)?;
    let props = build_writer_props(&schema);

    let mut buf = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props))?;

    for batch in batches {
        writer.write(batch)?;
    }

    writer.close()?;
    Ok(buf)
}

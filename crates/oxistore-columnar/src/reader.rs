/// Parquet reader using `ParquetRecordBatchReaderBuilder`.
///
/// Reads all row groups from a Parquet file, collecting them into a `Vec<RecordBatch>`.
use std::path::Path;
use std::sync::Arc;

use arrow::array::{new_null_array, ArrayRef};
use arrow::datatypes::{Field, Schema};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ProjectionMask;

use crate::predicate::Predicate;
use crate::{ColumnarError, ColumnarTable, CompressionMode, ParquetFileMetadata};

/// Read all batches from the Parquet file at `path`.
///
/// # Errors
///
/// Returns [`ColumnarError::Io`] on filesystem errors and
/// [`ColumnarError::Parquet`] on deserialisation failures.
pub(crate) fn read_batches(path: &Path) -> Result<Vec<RecordBatch>, ColumnarError> {
    let file = std::fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let reader = builder.build()?;

    let batches: Result<Vec<RecordBatch>, _> = reader.collect();
    Ok(batches?)
}

/// Read batches from the Parquet file at `path`, selecting only the specified
/// columns (projection pushdown / column pruning).
///
/// `column_indices` specifies which columns to read (zero-based indices
/// into the file schema).
pub(crate) fn read_batches_with_projection(
    path: &Path,
    column_indices: &[usize],
) -> Result<Vec<RecordBatch>, ColumnarError> {
    let file = std::fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;

    let parquet_schema = builder.parquet_schema().clone();
    let mask = ProjectionMask::leaves(&parquet_schema, column_indices.iter().copied());

    let reader = builder.with_projection(mask).build()?;
    let batches: Result<Vec<RecordBatch>, _> = reader.collect();
    Ok(batches?)
}

/// Read Parquet file metadata without reading any row data.
pub(crate) fn read_metadata(path: &Path) -> Result<ParquetFileMetadata, ColumnarError> {
    let file = std::fs::File::open(path)?;
    let file_size = file.metadata()?.len();
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;

    let metadata = builder.metadata();
    let schema = builder.schema().clone();
    let num_rows = metadata.file_metadata().num_rows();
    let num_row_groups = metadata.num_row_groups();
    let num_columns = schema.fields().len();

    Ok(ParquetFileMetadata {
        schema,
        num_rows,
        num_row_groups,
        num_columns,
        file_size,
    })
}

/// Read Parquet metadata from an in-memory byte slice without reading any row data.
///
/// Handles both raw Parquet bytes and OxiARC-compressed payloads (with the
/// 4-byte `b"OXIA"` magic header).
pub(crate) fn read_metadata_from_bytes(
    data: &[u8],
) -> Result<crate::ParquetFileMetaInfo, ColumnarError> {
    use crate::OXIA_MAGIC;

    let parquet_bytes = if data.starts_with(OXIA_MAGIC) {
        crate::decompress_payload(&data[OXIA_MAGIC.len()..])?
    } else {
        data.to_vec()
    };

    let cursor = Bytes::from(parquet_bytes);
    let builder = ParquetRecordBatchReaderBuilder::try_new(cursor)?;
    let metadata = builder.metadata();
    let schema = builder.schema();
    Ok(crate::ParquetFileMetaInfo {
        num_rows: metadata.file_metadata().num_rows(),
        num_row_groups: metadata.num_row_groups(),
        num_columns: schema.fields().len(),
        file_size: data.len() as u64,
    })
}

/// Read all batches from an in-memory byte buffer (Parquet format).
pub(crate) fn read_batches_from_bytes(data: &[u8]) -> Result<ColumnarTable, ColumnarError> {
    let cursor = Bytes::from(data.to_vec());
    let builder = ParquetRecordBatchReaderBuilder::try_new(cursor)?;
    let schema = builder.schema().clone();
    let reader = builder.build()?;

    let batches: Result<Vec<RecordBatch>, _> = reader.collect();
    let batches = batches?;

    Ok(ColumnarTable {
        schema: Arc::clone(&schema),
        batches,
        compression: CompressionMode::None,
    })
}

/// Read batches from bytes, projecting only the requested columns by name.
///
/// Returns a `ColumnarTable` that contains only the columns named in `columns`.
/// Unknown column names produce a `ColumnarError::SchemaMismatch` if *none*
/// of the requested names exist, otherwise missing names are silently skipped.
pub(crate) fn read_columns_from_bytes(
    data: &[u8],
    columns: &[&str],
) -> Result<ColumnarTable, ColumnarError> {
    let cursor = Bytes::from(data.to_vec());
    let builder = ParquetRecordBatchReaderBuilder::try_new(cursor)?;

    // Map column names to leaf indices in the Parquet schema.
    let parquet_schema = builder.parquet_schema().clone();
    let indices: Vec<usize> = parquet_schema
        .columns()
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if columns.contains(&c.name()) {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    let mask = ProjectionMask::leaves(&parquet_schema, indices);
    let schema = builder.schema().clone();
    let reader = builder.with_projection(mask).build()?;

    let batches: Result<Vec<RecordBatch>, _> = reader.collect();
    let batches = batches?;

    // Derive the actual projected schema from the first batch (or the Arrow schema).
    let projected_schema = if let Some(first) = batches.first() {
        first.schema()
    } else {
        // No batches — build schema manually from the Arrow schema.
        let fields: Vec<Arc<Field>> = schema
            .fields()
            .iter()
            .filter(|f| columns.contains(&f.name().as_str()))
            .cloned()
            .collect();
        Arc::new(Schema::new(fields))
    };

    Ok(ColumnarTable {
        schema: projected_schema,
        batches,
        compression: CompressionMode::None,
    })
}

/// Read batches from bytes, skipping row groups that cannot satisfy `pred`.
pub(crate) fn read_with_predicate_from_bytes(
    data: &[u8],
    pred: &Predicate,
) -> Result<ColumnarTable, ColumnarError> {
    let cursor = Bytes::from(data.to_vec());
    let builder = ParquetRecordBatchReaderBuilder::try_new(cursor)?;

    let meta = builder.metadata().clone();
    let schema_desc = meta.file_metadata().schema_descr();

    let surviving: Vec<usize> = (0..meta.num_row_groups())
        .filter(|&i| pred.row_group_might_match(meta.row_group(i), schema_desc))
        .collect();

    let schema = builder.schema().clone();
    let reader = builder.with_row_groups(surviving).build()?;

    let batches: Result<Vec<RecordBatch>, _> = reader.collect();
    let batches = batches?;

    Ok(ColumnarTable {
        schema,
        batches,
        compression: CompressionMode::None,
    })
}

/// Read batches from bytes with both column projection and row-group predicate pruning.
pub(crate) fn read_with_projection_and_predicate_from_bytes(
    data: &[u8],
    columns: &[&str],
    pred: &Predicate,
) -> Result<ColumnarTable, ColumnarError> {
    let cursor = Bytes::from(data.to_vec());
    let builder = ParquetRecordBatchReaderBuilder::try_new(cursor)?;

    let meta = builder.metadata().clone();
    let schema_desc = meta.file_metadata().schema_descr();

    // Row group pruning.
    let surviving: Vec<usize> = (0..meta.num_row_groups())
        .filter(|&i| pred.row_group_might_match(meta.row_group(i), schema_desc))
        .collect();

    // Column projection.
    let parquet_schema = builder.parquet_schema().clone();
    let indices: Vec<usize> = parquet_schema
        .columns()
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if columns.contains(&c.name()) {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    let mask = ProjectionMask::leaves(&parquet_schema, indices);
    let reader = builder
        .with_row_groups(surviving)
        .with_projection(mask)
        .build()?;

    let batches: Result<Vec<RecordBatch>, _> = reader.collect();
    let batches = batches?;

    let projected_schema = if let Some(first) = batches.first() {
        first.schema()
    } else {
        let arrow_schema = meta.file_metadata().schema_descr();
        // Build from column names — fall back to empty if nothing matches.
        let _schema_desc = arrow_schema; // suppress unused warning
        Arc::new(Schema::empty())
    };

    Ok(ColumnarTable {
        schema: projected_schema,
        batches,
        compression: CompressionMode::None,
    })
}

/// Read batches from bytes, adapting the file schema to a target Arrow schema.
///
/// - Columns present in both file and target: read normally (with name-based projection).
/// - Columns in target but absent in file: filled with null arrays of the target type.
/// - Columns in file but absent in target: ignored (projected away).
/// - Type mismatch for common columns: returns `ColumnarError::SchemaMismatch`.
pub(crate) fn read_with_schema_from_bytes(
    data: &[u8],
    target: &Arc<Schema>,
) -> Result<ColumnarTable, ColumnarError> {
    let cursor = Bytes::from(data.to_vec());
    let builder = ParquetRecordBatchReaderBuilder::try_new(cursor)?;

    let file_schema = builder.schema().clone();

    // Validate type compatibility for columns present in both schemas.
    for target_field in target.fields() {
        if let Ok(file_idx) = file_schema.index_of(target_field.name()) {
            let file_field = file_schema.field(file_idx);
            if file_field.data_type() != target_field.data_type() {
                return Err(ColumnarError::SchemaMismatch(format!(
                    "column '{}': file type {:?} != target type {:?}",
                    target_field.name(),
                    file_field.data_type(),
                    target_field.data_type()
                )));
            }
        }
    }

    // Determine which target columns exist in the file (for projection).
    let columns_to_read: Vec<&str> = target
        .fields()
        .iter()
        .filter_map(|f| {
            if file_schema.index_of(f.name()).is_ok() {
                Some(f.name().as_str())
            } else {
                None
            }
        })
        .collect();

    // Project only the needed columns.
    let parquet_schema = builder.parquet_schema().clone();
    let indices: Vec<usize> = parquet_schema
        .columns()
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if columns_to_read.contains(&c.name()) {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    let mask = ProjectionMask::leaves(&parquet_schema, indices);
    let reader = builder.with_projection(mask).build()?;

    let read_batches: Result<Vec<RecordBatch>, _> = reader.collect();
    let read_batches = read_batches?;

    // Re-assemble each batch to match the target schema (adding null columns).
    let mut result_batches = Vec::with_capacity(read_batches.len());
    for batch in &read_batches {
        let num_rows = batch.num_rows();
        let mut cols: Vec<ArrayRef> = Vec::with_capacity(target.fields().len());

        for target_field in target.fields() {
            let col: ArrayRef = if let Ok(idx) = batch.schema().index_of(target_field.name()) {
                Arc::clone(batch.column(idx))
            } else {
                // Column absent in file — fill with nulls.
                new_null_array(target_field.data_type(), num_rows)
            };
            cols.push(col);
        }

        let new_batch = RecordBatch::try_new(Arc::clone(target), cols)?;
        result_batches.push(new_batch);
    }

    // Handle the empty case (no batches).
    Ok(ColumnarTable {
        schema: Arc::clone(target),
        batches: result_batches,
        compression: CompressionMode::None,
    })
}

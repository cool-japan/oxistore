/// Streaming Parquet writer and reader.
///
/// `ColumnarStreamWriter` wraps `ArrowWriter` for incremental batch writing
/// without materializing the entire dataset in memory first.
///
/// `ColumnarStreamReader` wraps `ParquetRecordBatchReader` and exposes it as
/// an `Iterator<Item = Result<RecordBatch, ColumnarError>>`, yielding batches
/// lazily from an in-memory byte buffer.
use std::io::Write;
use std::sync::Arc;

use arrow::array::RecordBatchReader;
use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::arrow_writer::ArrowWriter;
use parquet::file::properties::WriterProperties;

use crate::ColumnarError;

/// An incremental Parquet writer that accepts one [`RecordBatch`] at a time.
///
/// Unlike the bulk writer functions this type does not require all batches to
/// be present before writing begins.  Each call to [`Self::write_batch`] serialises
/// the batch into the underlying writer immediately.
///
/// Call [`Self::finish`] to flush any buffered state and finalise the Parquet footer.
///
/// # Type parameter
///
/// `W` must implement both [`Write`] and [`Send`] (the Arrow writer requires
/// `Send` for some internal operations on certain platforms).
pub struct ColumnarStreamWriter<W: Write + Send> {
    writer: ArrowWriter<W>,
}

impl<W: Write + Send> ColumnarStreamWriter<W> {
    /// Create a new streaming writer.
    ///
    /// - `schema` — the Arrow schema that all batches must conform to.
    /// - `sink`   — the byte sink to write Parquet data into.
    /// - `props`  — optional [`WriterProperties`]; falls back to the
    ///   enhanced defaults from `crate::writer::build_writer_props` when
    ///   `None`.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::Parquet`] if the writer cannot be initialised
    /// (e.g. the sink is not writable or the schema contains unsupported types).
    pub fn new(
        schema: Arc<Schema>,
        sink: W,
        props: Option<WriterProperties>,
    ) -> Result<Self, ColumnarError> {
        let effective_props = props.unwrap_or_else(|| crate::writer::build_writer_props(&schema));
        let writer = ArrowWriter::try_new(sink, Arc::clone(&schema), Some(effective_props))?;
        Ok(ColumnarStreamWriter { writer })
    }

    /// Write one batch to the underlying Parquet writer.
    ///
    /// The batch is serialised immediately; no additional buffering is applied
    /// on top of what `ArrowWriter` already performs internally.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::Parquet`] on serialisation failure or
    /// [`ColumnarError::Arrow`] on schema validation failure.
    pub fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), ColumnarError> {
        self.writer.write(batch).map_err(ColumnarError::Parquet)
    }

    /// Finalise the writer and flush the Parquet footer.
    ///
    /// After calling this method the writer is consumed and the underlying sink
    /// will contain a complete, valid Parquet file.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::Parquet`] if closing the writer fails.
    pub fn finish(self) -> Result<(), ColumnarError> {
        self.writer
            .close()
            .map(|_| ())
            .map_err(ColumnarError::Parquet)
    }
}

/// A lazy Parquet reader that yields [`RecordBatch`]es one at a time.
///
/// The reader is backed by an in-memory byte buffer so no filesystem I/O is
/// performed after construction.  Batches are decoded on demand as the
/// iterator is advanced.
pub struct ColumnarStreamReader {
    inner: parquet::arrow::arrow_reader::ParquetRecordBatchReader,
}

impl ColumnarStreamReader {
    /// Construct a reader from a Parquet byte buffer.
    ///
    /// The bytes are read lazily — the Parquet footer is parsed during
    /// construction and row data is decoded as the iterator is advanced.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::Parquet`] if the buffer is not a valid Parquet
    /// file (footer parse failure, unsupported encoding, etc.).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, ColumnarError> {
        let cursor = Bytes::from(bytes);
        let builder = ParquetRecordBatchReaderBuilder::try_new(cursor)?;
        let inner = builder.build()?;
        Ok(ColumnarStreamReader { inner })
    }

    /// Return the Arrow schema inferred from the Parquet file.
    pub fn schema(&self) -> Arc<Schema> {
        self.inner.schema()
    }
}

impl Iterator for ColumnarStreamReader {
    type Item = Result<RecordBatch, ColumnarError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|r| r.map_err(ColumnarError::Arrow))
    }
}

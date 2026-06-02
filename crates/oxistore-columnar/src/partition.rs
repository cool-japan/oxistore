//! Hive-style multi-column partitioned dataset support.
//!
//! Writes logical tables as multiple Parquet files organised into nested
//! `<col1>=<v1>/<col2>=<v2>/` directories (Hive layout), with a TSV manifest
//! for fast partition discovery and optional pruning on read.
//!
//! # Layout on disk
//!
//! **Single-column (v1 manifest, backwards-compatible):**
//! ```text
//! <root>/
//!   manifest.tsv               — header: partition_value\trel_path\trow_count
//!                                data:   <v>\t<path>\t<count>
//!   <col>=<encoded_value>/
//!     part-0000.parquet
//! ```
//!
//! **Multi-column (v2 manifest):**
//! ```text
//! <root>/
//!   manifest.tsv               — first line: manifest_version=2
//!                                second line: <col1>\t<col2>\t...\trel_path\trow_count
//!                                data:        <v1>\t<v2>\t...\t<path>\t<count>
//!   <col1>=<v1>/<col2>=<v2>/
//!     part-0000.parquet
//! ```
//!
//! # Partition column encoding
//!
//! Partition values are encoded for safe use as directory names: all
//! characters except alphanumerics, `-`, `.`, `+`, and `_` are replaced
//! with `_`.  This encoding is **not** reversible in the general case —
//! the original value is always read from the manifest, never reconstructed
//! from the directory name.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::StringArray;
use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;

use crate::{ColumnarError, ColumnarTable, CompressionMode};

// ── Manifest ─────────────────────────────────────────────────────────────────

/// A single row in `manifest.tsv` (v2 format).
///
/// For v1 compatibility the `partition_values` Vec has exactly one element.
struct ManifestEntry {
    /// The raw (un-encoded) partition column values, in column order.
    partition_values: Vec<String>,
    /// Path relative to the dataset root (e.g. `year=2024/month=01/part-0000.parquet`).
    rel_path: String,
    /// Total number of rows in this partition file.
    row_count: usize,
}

// ── Partition encoding ────────────────────────────────────────────────────────

/// Encode a partition value for use as part of a directory name.
///
/// Characters that are unsafe in directory names on common filesystems are
/// replaced with `_`.  The encoded form is used only for the directory path;
/// the original value is stored verbatim in `manifest.tsv`.
fn encode_partition_value(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '.' | '+' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ── PartitionPredicate ────────────────────────────────────────────────────────

/// A predicate used to prune partitions at read time.
///
/// Only partitions whose value(s) match the predicate are read from disk.
/// Partitions that do not match are skipped entirely — no I/O is performed
/// for them.
///
/// For multi-column datasets use the [`And`](PartitionPredicate::And) variant,
/// which lets you express per-column constraints.
#[derive(Debug, Clone)]
pub enum PartitionPredicate {
    /// Include only partitions where the (single) partition value equals this
    /// string exactly.
    Eq(String),
    /// Include only partitions whose (single) partition value is a member of
    /// this set.
    In(Vec<String>),
    /// Include only partitions whose (single) partition value satisfies
    /// `lo <= value < hi` (lexicographic comparison).
    Range {
        /// Inclusive lower bound.
        lo: String,
        /// Exclusive upper bound.
        hi: String,
    },
    /// Conjunctive multi-column predicate: each element is
    /// `(column_name, sub_predicate)`.  A partition entry passes if and only
    /// if **all** per-column sub-predicates match the entry's value for that
    /// column.
    And(Vec<(String, PartitionPredicate)>),
}

impl PartitionPredicate {
    /// Return `true` if `value` satisfies this leaf predicate.
    ///
    /// Panics if called on [`And`](Self::And) — `And` is evaluated by
    /// `matches_multi` against a full partition tuple.
    fn matches_single(&self, value: &str) -> bool {
        match self {
            Self::Eq(v) => value == v.as_str(),
            Self::In(vs) => vs.iter().any(|v| v.as_str() == value),
            Self::Range { lo, hi } => value >= lo.as_str() && value < hi.as_str(),
            Self::And(_) => panic!("matches_single called on And predicate; use matches_multi"),
        }
    }

    /// Return `true` if this predicate matches the given partition tuple.
    ///
    /// `column_names` and `values` are in the same order as the manifest columns.
    fn matches_multi(&self, column_names: &[String], values: &[String]) -> bool {
        match self {
            Self::And(pairs) => pairs.iter().all(|(col_name, sub_pred)| {
                // Find the index of this column.
                let idx = column_names.iter().position(|c| c == col_name);
                match idx {
                    Some(i) => {
                        sub_pred.matches_single(values.get(i).map(String::as_str).unwrap_or(""))
                    }
                    // Column not present → treat as non-match.
                    None => false,
                }
            }),
            // Single-column predicates: use the first (and only) value.
            other => other.matches_single(values.first().map(String::as_str).unwrap_or("")),
        }
    }
}

// ── PartitionedDataset ────────────────────────────────────────────────────────

/// A logical table stored as multiple Parquet files, one per partition tuple.
///
/// Data is organised in Hive-style directories:
/// `<root>/<col1>=<v1>/<col2>=<v2>/part-0000.parquet`.
/// A `manifest.tsv` file at the root records partition values, relative file
/// paths, and row counts for fast discovery and pruning.
///
/// # Single-column (v1 backwards-compatible) vs. multi-column (v2)
///
/// Use [`new_single_column`] or the v1-compatible [`new`] overload to create a
/// dataset partitioned by exactly one column — this writes a v1 manifest that
/// is compatible with older readers.
///
/// Use [`new`] with a `Vec<String>` containing multiple column names to create
/// a multi-column (v2) dataset.  The v2 manifest is prefixed with a
/// `manifest_version=2` line so that newer readers can distinguish it from v1.
///
/// The reader always auto-detects the manifest format: if the first line starts
/// with `manifest_version=`, it is treated as v2; otherwise as v1.
///
/// [`new_single_column`]: PartitionedDataset::new_single_column
/// [`new`]: PartitionedDataset::new
///
/// # Compression
///
/// By default no outer compression is applied (`CompressionMode::None`).
/// When `CompressionMode::OxiArc` is set (requires the `compress` feature),
/// each partition file is written as an OxiARC-compressed Parquet payload.
/// The reader detects the magic header automatically.
pub struct PartitionedDataset {
    /// Root directory that contains all partition subdirectories.
    root: PathBuf,
    /// Names of the columns used to partition the data (in order).
    partition_columns: Vec<String>,
    /// Outer compression mode applied to each partition file.
    compression: CompressionMode,
}

impl PartitionedDataset {
    /// Create a new `PartitionedDataset` rooted at `root`, partitioned by the
    /// given columns.
    ///
    /// When `partition_columns` contains exactly one element, the written
    /// manifest is the legacy v1 format (for backwards compatibility with older
    /// readers).  When it contains two or more elements the manifest uses v2
    /// format with a `manifest_version=2` header.
    ///
    /// No compression is applied by default.  Call [`with_compression`] to
    /// enable OxiARC DEFLATE payload compression (requires the `compress`
    /// feature).
    ///
    /// [`with_compression`]: PartitionedDataset::with_compression
    #[must_use]
    pub fn new(root: PathBuf, partition_columns: Vec<String>) -> Self {
        Self {
            root,
            partition_columns,
            compression: CompressionMode::None,
        }
    }

    /// Convenience constructor for datasets partitioned by a single column.
    ///
    /// Equivalent to `PartitionedDataset::new(root, vec![col.into()])`.  The
    /// written manifest is the legacy v1 format (no `manifest_version=` header).
    #[must_use]
    pub fn new_single_column(root: PathBuf, col: impl Into<String>) -> Self {
        Self::new(root, vec![col.into()])
    }

    /// Set the OxiARC compression mode for partition files.
    ///
    /// This replaces the current mode (including `None`).  Pass
    /// `CompressionMode::OxiArc { level }` to enable compression (requires
    /// the `compress` feature to take effect).
    #[must_use]
    pub fn with_compression(mut self, compression: CompressionMode) -> Self {
        self.compression = compression;
        self
    }

    /// Return the root directory of this dataset.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return the names of the partition columns.
    #[must_use]
    pub fn partition_columns(&self) -> &[String] {
        &self.partition_columns
    }

    /// Write a collection of record batches, partitioned by `partition_columns`.
    ///
    /// Each unique combination of partition column values gets its own
    /// subdirectory and a single `part-0000.parquet` file.  A `manifest.tsv`
    /// is written (or overwritten) at the dataset root on success.
    ///
    /// The partition columns are **retained** in the written Parquet files so
    /// that readers can reconstruct the full schema without consulting the
    /// directory name.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::SchemaMismatch`] if any `partition_column` is
    /// not found in the batch schema, or if any partition column is not of type
    /// `Utf8`.  Returns [`ColumnarError::Io`] on filesystem failures.
    /// Returns [`ColumnarError::Parquet`] on serialisation failures.
    pub fn write_partitioned(&self, batches: &[RecordBatch]) -> Result<(), ColumnarError> {
        // Group batches by partition tuple (Vec<String>).
        let mut groups: HashMap<Vec<String>, Vec<RecordBatch>> = HashMap::new();

        for batch in batches {
            let schema = batch.schema();

            // Locate each partition column and verify it is Utf8.
            let col_indices: Vec<usize> = self
                .partition_columns
                .iter()
                .map(|col_name| {
                    schema.index_of(col_name).map_err(|_| {
                        ColumnarError::SchemaMismatch(format!(
                            "partition column '{col_name}' not found in schema"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;

            // Downcast each partition column to StringArray.
            let str_cols: Vec<&StringArray> = col_indices
                .iter()
                .zip(self.partition_columns.iter())
                .map(|(&idx, col_name)| {
                    let col = batch.column(idx);
                    col.as_any().downcast_ref::<StringArray>().ok_or_else(|| {
                        ColumnarError::SchemaMismatch(format!(
                            "partition column '{col_name}' must be Utf8 (String), got {:?}",
                            col.data_type()
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;

            // Walk every row and collect the partition tuple.
            let num_rows = batch.num_rows();
            let mut row_groups: HashMap<Vec<String>, Vec<bool>> = HashMap::new();

            for row in 0..num_rows {
                let tuple: Vec<String> = str_cols
                    .iter()
                    .map(|col| col.value(row).to_string())
                    .collect();
                let entry = row_groups
                    .entry(tuple)
                    .or_insert_with(|| vec![false; num_rows]);
                entry[row] = true;
            }

            for (tuple, mask_vec) in row_groups {
                let mask = arrow::array::BooleanArray::from(mask_vec);
                let filtered = arrow::compute::filter_record_batch(batch, &mask)?;
                groups.entry(tuple).or_default().push(filtered);
            }
        }

        fs::create_dir_all(&self.root)?;

        let mut manifest_entries: Vec<ManifestEntry> = Vec::with_capacity(groups.len());

        for (tuple, group_batches) in &groups {
            // Build the nested directory path: col1=v1/col2=v2/...
            let dir_parts: Vec<String> = self
                .partition_columns
                .iter()
                .zip(tuple.iter())
                .map(|(col, val)| format!("{}={}", col, encode_partition_value(val)))
                .collect();
            let dir_rel: String = dir_parts.join("/");
            let dir_path = self.root.join(&dir_rel);
            fs::create_dir_all(&dir_path)?;

            let file_path = dir_path.join("part-0000.parquet");

            // Derive the schema from the first batch in the group.
            let schema: Arc<Schema> = group_batches
                .first()
                .map(|b| b.schema())
                .unwrap_or_else(|| Arc::new(Schema::empty()));

            let total_rows =
                write_group_to_file(&file_path, schema, group_batches, self.compression)?;

            let rel_path = format!("{dir_rel}/part-0000.parquet");
            manifest_entries.push(ManifestEntry {
                partition_values: tuple.clone(),
                rel_path,
                row_count: total_rows,
            });
        }

        self.write_manifest(&manifest_entries)?;
        Ok(())
    }

    /// Read all matching partitions from disk, applying optional partition pruning.
    ///
    /// When `predicate` is `None` all partitions listed in `manifest.tsv` are
    /// read.  When `predicate` is `Some(p)` only partitions whose value(s)
    /// satisfy `p` are read; all others are skipped entirely (no I/O).
    ///
    /// Use `PartitionPredicate::Eq` / `In` / `Range` for single-column datasets
    /// and `PartitionPredicate::And` for multi-column datasets.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::Manifest`] if the manifest is missing or
    /// malformed.  Returns [`ColumnarError::Io`] or [`ColumnarError::Parquet`]
    /// on file read failures.
    pub fn read_partitioned(
        &self,
        predicate: Option<&PartitionPredicate>,
    ) -> Result<Vec<RecordBatch>, ColumnarError> {
        let (column_names, entries) = self.read_manifest()?;

        let mut all_batches: Vec<RecordBatch> = Vec::new();
        for entry in &entries {
            if let Some(pred) = predicate {
                if !pred.matches_multi(&column_names, &entry.partition_values) {
                    continue;
                }
            }
            let file_path = self.root.join(&entry.rel_path);
            let batches = read_group_from_file(&file_path)?;
            all_batches.extend(batches);
        }
        Ok(all_batches)
    }

    /// Return the partition entries listed in `manifest.tsv` without reading
    /// any row data.
    ///
    /// Each entry is `(partition_values, rel_path, row_count)`.  For
    /// single-column datasets the inner `Vec<String>` has exactly one element.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError::Manifest`] if the manifest is absent or
    /// contains malformed lines.
    pub fn list_partitions(&self) -> Result<Vec<(Vec<String>, String, usize)>, ColumnarError> {
        let (_column_names, entries) = self.read_manifest()?;
        Ok(entries
            .into_iter()
            .map(|e| (e.partition_values, e.rel_path, e.row_count))
            .collect())
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Write the manifest in the appropriate format:
    /// - v1 (single-column, no version header) when `partition_columns.len() == 1`
    /// - v2 (multi-column, `manifest_version=2` header) otherwise
    fn write_manifest(&self, entries: &[ManifestEntry]) -> Result<(), ColumnarError> {
        let path = self.root.join("manifest.tsv");
        let mut f = fs::File::create(&path)?;

        let is_multi = self.partition_columns.len() != 1;

        if is_multi {
            writeln!(f, "manifest_version=2")?;
            // Header: col1\tcol2\t...\trel_path\trow_count
            let header = self
                .partition_columns
                .iter()
                .map(String::as_str)
                .chain(["rel_path", "row_count"])
                .collect::<Vec<_>>()
                .join("\t");
            writeln!(f, "{header}")?;
            for e in entries {
                let vals = e.partition_values.join("\t");
                writeln!(f, "{vals}\t{}\t{}", e.rel_path, e.row_count)?;
            }
        } else {
            // v1 format — backwards-compatible with old readers.
            writeln!(f, "partition_value\trel_path\trow_count")?;
            for e in entries {
                let val = e.partition_values.first().map(String::as_str).unwrap_or("");
                writeln!(f, "{val}\t{}\t{}", e.rel_path, e.row_count)?;
            }
        }
        Ok(())
    }

    /// Parse `manifest.tsv`, auto-detecting v1 vs v2 format.
    ///
    /// Returns `(column_names, entries)`.  For v1 manifests `column_names` is
    /// a Vec of length 1 containing the column name that was passed to `new` at
    /// write time (we reconstruct it from the current `partition_columns`).
    fn read_manifest(&self) -> Result<(Vec<String>, Vec<ManifestEntry>), ColumnarError> {
        let path = self.root.join("manifest.tsv");
        let f = fs::File::open(&path)?;
        let reader = BufReader::new(f);
        let mut lines = reader.lines();

        // Read the first line to detect the format.
        let first_line = lines
            .next()
            .ok_or_else(|| ColumnarError::Manifest("manifest.tsv is empty".to_string()))??;

        if first_line.starts_with("manifest_version=") {
            // ── v2 format ──────────────────────────────────────────────────
            // Next line is the header: col1\tcol2\t...\trel_path\trow_count
            let header_line = lines.next().ok_or_else(|| {
                ColumnarError::Manifest("v2 manifest has no header line".to_string())
            })??;
            let headers: Vec<&str> = header_line.split('\t').collect();
            // The last two header fields are always "rel_path" and "row_count".
            if headers.len() < 3 {
                return Err(ColumnarError::Manifest(format!(
                    "v2 manifest header has only {} fields (minimum 3)",
                    headers.len()
                )));
            }
            let n_cols = headers.len() - 2; // number of partition columns
            let col_names: Vec<String> = headers[..n_cols].iter().map(|s| s.to_string()).collect();

            let mut entries: Vec<ManifestEntry> = Vec::new();
            for (line_idx, line_result) in lines.enumerate() {
                let line = line_result?;
                let parts: Vec<&str> = line.splitn(n_cols + 3, '\t').collect();
                // Expected: n_cols values + rel_path + row_count = n_cols + 2
                if parts.len() < n_cols + 2 {
                    return Err(ColumnarError::Manifest(format!(
                        "v2 manifest line {}: expected {} tab-separated fields, got {}",
                        line_idx + 3,
                        n_cols + 2,
                        parts.len()
                    )));
                }
                let partition_values: Vec<String> =
                    parts[..n_cols].iter().map(|s| s.to_string()).collect();
                let rel_path = parts[n_cols].to_string();
                let row_count = parts[n_cols + 1].parse::<usize>().map_err(|_| {
                    ColumnarError::Manifest(format!(
                        "v2 manifest line {}: invalid row_count '{}'",
                        line_idx + 3,
                        parts[n_cols + 1]
                    ))
                })?;
                entries.push(ManifestEntry {
                    partition_values,
                    rel_path,
                    row_count,
                });
            }
            Ok((col_names, entries))
        } else {
            // ── v1 format ──────────────────────────────────────────────────
            // first_line is the header row: "partition_value\trel_path\trow_count"
            // (we skip it and use our own column name from partition_columns)
            let col_name = self
                .partition_columns
                .first()
                .cloned()
                .unwrap_or_else(|| "partition_value".to_string());

            let mut entries: Vec<ManifestEntry> = Vec::new();
            for (line_idx, line_result) in lines.enumerate() {
                let line = line_result?;
                let parts: Vec<&str> = line.splitn(3, '\t').collect();
                if parts.len() != 3 {
                    return Err(ColumnarError::Manifest(format!(
                        "manifest line {}: expected 3 tab-separated fields, got {}",
                        line_idx + 2,
                        parts.len()
                    )));
                }
                let row_count = parts[2].parse::<usize>().map_err(|_| {
                    ColumnarError::Manifest(format!(
                        "manifest line {}: invalid row_count '{}'",
                        line_idx + 2,
                        parts[2]
                    ))
                })?;
                entries.push(ManifestEntry {
                    partition_values: vec![parts[0].to_string()],
                    rel_path: parts[1].to_string(),
                    row_count,
                });
            }
            Ok((vec![col_name], entries))
        }
    }
}

// ── File I/O helpers ──────────────────────────────────────────────────────────

/// Write `batches` to `path`, applying `compression` via the OxiARC envelope
/// when set.  Returns the total number of rows written.
fn write_group_to_file(
    path: &Path,
    schema: Arc<Schema>,
    batches: &[RecordBatch],
    compression: CompressionMode,
) -> Result<usize, ColumnarError> {
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();

    match compression {
        CompressionMode::None => {
            // Write a raw Parquet file directly.
            crate::write_batches(path, schema, batches)?;
        }
        CompressionMode::OxiArc { level } => {
            // Write via in-memory buffer so we can apply the OxiARC envelope.
            let mut table = ColumnarTable::new(schema);
            for batch in batches {
                table.push_unchecked(batch.clone());
            }
            let bytes = table.with_compression(level).write_to_bytes()?;
            fs::write(path, bytes)?;
        }
    }

    Ok(total_rows)
}

/// Read all batches from `path`, handling both raw Parquet and OxiARC-compressed
/// payloads transparently.
fn read_group_from_file(path: &Path) -> Result<Vec<RecordBatch>, ColumnarError> {
    // Read raw bytes first so we can check for the OXIA magic header.
    let bytes = fs::read(path)?;
    if bytes.starts_with(b"OXIA") {
        // OxiARC-compressed payload — use the table reader which handles inflation.
        let table = ColumnarTable::read_from_bytes(&bytes)?;
        Ok(table.batches)
    } else {
        // Plain Parquet — use the direct file reader (avoids an extra copy).
        crate::read_batches(path)
    }
}

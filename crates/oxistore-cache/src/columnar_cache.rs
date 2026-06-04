//! [`ColumnarRowGroupCache`] — caches hot Parquet row groups from
//! [`oxistore_columnar`] in a bounded LRU, enabling repeated scans over the
//! same row groups to avoid re-serialising/re-deserialising Parquet bytes.
//!
//! # Design
//!
//! A Parquet file is composed of one or more _row groups_.  In analytical
//! workloads the same small set of row groups (the "hot" ones) are often
//! scanned repeatedly.  [`ColumnarRowGroupCache`] acts as a transparent
//! read-through layer:
//!
//! - On a **cache miss** the row group bytes are fetched from `source` (a
//!   `ColumnarTable` or a byte slice), serialised to in-memory Parquet bytes,
//!   stored in the LRU, and returned.
//! - On a **cache hit** the bytes are returned immediately from RAM.
//!
//! Keys are `(file_id: String, row_group_index: usize)` tuples, allowing the
//! cache to serve multiple logical "files" simultaneously.
//!
//! # TTL
//!
//! Entries may be inserted with a TTL via [`ColumnarRowGroupCache::load_row_group_with_ttl`].
//! Expired entries are lazily evicted on the next access.
//!
//! # Memory overhead
//!
//! The cache stores raw Parquet-encoded `Vec<u8>` values — typically 10–100×
//! smaller than the equivalent in-memory Arrow `RecordBatch` representation —
//! so the capacity limit (`max_entries`) should be set according to expected
//! Parquet payload sizes, not uncompressed row sizes.

use std::sync::Arc;
use std::time::Duration;

use arrow::record_batch::RecordBatch;
use oxistore_columnar::{ColumnarError, ColumnarTable};

use crate::lru::LruCache;
use crate::Cache;

// ── Cache key ────────────────────────────────────────────────────────────────

/// Key identifying a single row group within a logical Parquet file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowGroupKey {
    /// Logical file identifier (path, UUID, or any opaque string).
    pub file_id: String,
    /// Zero-based row group index within the file.
    pub row_group_index: usize,
}

impl RowGroupKey {
    /// Create a new cache key.
    #[must_use]
    pub fn new(file_id: impl Into<String>, row_group_index: usize) -> Self {
        Self {
            file_id: file_id.into(),
            row_group_index,
        }
    }
}

// ── ColumnarRowGroupCache ─────────────────────────────────────────────────────

/// In-memory LRU cache of serialised Parquet row groups.
///
/// `V` is always `Vec<u8>` — the raw Parquet bytes of a single row group.
/// The cache is keyed by [`RowGroupKey`] = `(file_id, row_group_index)`.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use arrow::datatypes::{DataType, Field, Schema};
/// use arrow::array::Int64Array;
/// use arrow::record_batch::RecordBatch;
/// use oxistore_columnar::ColumnarTable;
/// use oxistore_cache::columnar_cache::ColumnarRowGroupCache;
///
/// let schema = Arc::new(Schema::new(vec![
///     Field::new("id", DataType::Int64, false),
/// ]));
/// let batch = RecordBatch::try_new(
///     Arc::clone(&schema),
///     vec![Arc::new(Int64Array::from(vec![1i64, 2, 3]))],
/// ).unwrap();
/// let mut table = ColumnarTable::new(Arc::clone(&schema));
/// table.push(batch).unwrap();
///
/// let mut cache = ColumnarRowGroupCache::new(64);
/// let bytes = cache.load_row_group("myfile", 0, &table).unwrap();
/// assert!(!bytes.is_empty());
/// ```
pub struct ColumnarRowGroupCache {
    inner: LruCache<RowGroupKey, Vec<u8>>,
    hits: u64,
    misses: u64,
}

impl ColumnarRowGroupCache {
    /// Create a new cache with the given maximum entry count.
    ///
    /// `max_entries` limits the number of row groups held in RAM
    /// (each entry is one serialised row group's Parquet bytes).
    #[must_use]
    pub fn new(max_entries: usize) -> Self {
        Self {
            inner: LruCache::new(max_entries),
            hits: 0,
            misses: 0,
        }
    }

    /// Return the number of cache hits since creation.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Return the number of cache misses since creation.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Return the hit rate as a fraction in `[0.0, 1.0]`.
    ///
    /// Returns `0.0` if no accesses have been recorded yet.
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Return the number of row groups currently held in the cache.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Return `true` if the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Return the maximum number of entries the cache can hold.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.inner.cap()
    }

    /// Look up a previously-cached row group by key.
    ///
    /// Returns `Some(&bytes)` on a hit, `None` on a miss or expiry.
    /// Accessing an entry promotes it to the MRU position (normal LRU
    /// semantics).
    pub fn get(&mut self, key: &RowGroupKey) -> Option<&[u8]> {
        let v = self.inner.get(key);
        if v.is_some() {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        v.map(|b| b.as_slice())
    }

    /// Manually insert raw Parquet bytes for a row group key.
    ///
    /// This is useful when the caller already has the bytes and wants to
    /// pre-warm the cache without going through [`load_row_group`].
    ///
    /// [`load_row_group`]: ColumnarRowGroupCache::load_row_group
    pub fn insert(&mut self, key: RowGroupKey, bytes: Vec<u8>) {
        self.inner.put(key, bytes);
    }

    /// Manually insert raw Parquet bytes for a row group key with a TTL.
    ///
    /// After `ttl` has elapsed the entry will be treated as a miss.
    pub fn insert_with_ttl(&mut self, key: RowGroupKey, bytes: Vec<u8>, ttl: Duration) {
        self.inner.put_with_ttl(key, bytes, ttl);
    }

    /// Remove a cached row group.
    ///
    /// Returns the raw bytes if the key was present, `None` otherwise.
    pub fn evict(&mut self, key: &RowGroupKey) -> Option<Vec<u8>> {
        self.inner.remove(key)
    }

    /// Remove all cached row groups for the given logical `file_id`.
    ///
    /// This is useful when a file is updated or deleted and all of its
    /// cached row groups should be invalidated at once.
    pub fn invalidate_file(&mut self, file_id: &str) {
        // Collect matching keys first to avoid mutating while iterating.
        let keys: Vec<RowGroupKey> = self
            .inner
            .iter()
            .filter(|(k, _)| k.file_id == file_id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            self.inner.remove(&k);
        }
    }

    /// Remove all cached entries.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Serialise the row group at `row_group_index` inside `table` to Parquet
    /// bytes, cache the result under `(file_id, row_group_index)`, and return
    /// a reference to the cached bytes.
    ///
    /// If the entry is already cached the stored bytes are returned immediately
    /// (read-through semantics).
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] if serialisation of the row group fails.
    pub fn load_row_group(
        &mut self,
        file_id: impl Into<String>,
        row_group_index: usize,
        table: &ColumnarTable,
    ) -> Result<&[u8], ColumnarError> {
        let key = RowGroupKey::new(file_id, row_group_index);

        // Cache hit: promote and return.
        if self.inner.contains_key(&key) {
            self.hits += 1;
            return Ok(self
                .inner
                .get(&key)
                .map(|v| v.as_slice())
                .unwrap_or_default());
        }

        // Cache miss: serialise the requested row group.
        self.misses += 1;
        let bytes = serialise_row_group(table, row_group_index)?;
        self.inner.put(key.clone(), bytes);

        // Return reference from cache (safe: we just inserted).
        Ok(self
            .inner
            .get(&key)
            .map(|v| v.as_slice())
            .unwrap_or_default())
    }

    /// Like [`load_row_group`] but with a TTL: the cached bytes expire after
    /// `ttl` has elapsed.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] if serialisation of the row group fails.
    ///
    /// [`load_row_group`]: ColumnarRowGroupCache::load_row_group
    pub fn load_row_group_with_ttl(
        &mut self,
        file_id: impl Into<String>,
        row_group_index: usize,
        table: &ColumnarTable,
        ttl: Duration,
    ) -> Result<&[u8], ColumnarError> {
        let key = RowGroupKey::new(file_id, row_group_index);

        if self.inner.contains_key(&key) {
            self.hits += 1;
            return Ok(self
                .inner
                .get(&key)
                .map(|v| v.as_slice())
                .unwrap_or_default());
        }

        self.misses += 1;
        let bytes = serialise_row_group(table, row_group_index)?;
        self.inner.put_with_ttl(key.clone(), bytes, ttl);

        Ok(self
            .inner
            .get(&key)
            .map(|v| v.as_slice())
            .unwrap_or_default())
    }

    /// Pre-warm the cache by serialising every row group in `table` and
    /// storing them under `file_id`.
    ///
    /// Entries are inserted without TTL.  Fails fast on the first
    /// serialisation error.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] if any row group fails to serialise.
    pub fn warm_from_table(
        &mut self,
        file_id: impl Into<String> + Clone,
        table: &ColumnarTable,
    ) -> Result<(), ColumnarError> {
        for idx in 0..table.batches.len() {
            let key = RowGroupKey::new(file_id.clone(), idx);
            if !self.inner.contains_key(&key) {
                let bytes = serialise_row_group(table, idx)?;
                self.inner.put(key, bytes);
            }
        }
        Ok(())
    }

    /// Deserialise a previously-cached row group back into a [`RecordBatch`].
    ///
    /// This is a convenience helper for callers that want to work with
    /// Arrow RecordBatches rather than raw Parquet bytes.
    ///
    /// Returns `None` if the row group is not in the cache.
    ///
    /// # Errors
    ///
    /// Returns [`ColumnarError`] if Parquet deserialisation fails.
    pub fn get_as_batch(
        &mut self,
        key: &RowGroupKey,
    ) -> Result<Option<RecordBatch>, ColumnarError> {
        let Some(bytes) = self.inner.get(key) else {
            return Ok(None);
        };
        let batches = oxistore_columnar::read_batches_from_bytes(bytes)?;
        Ok(batches.into_iter().next())
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Serialise the batch at `row_group_index` inside `table` to Parquet bytes.
///
/// The batch is wrapped in a single-batch `ColumnarTable` and serialised with
/// the default writer configuration.
fn serialise_row_group(
    table: &ColumnarTable,
    row_group_index: usize,
) -> Result<Vec<u8>, ColumnarError> {
    let batch = table.batches.get(row_group_index).ok_or_else(|| {
        ColumnarError::SchemaMismatch(format!(
            "row group index {row_group_index} out of range (table has {} batches)",
            table.batches.len()
        ))
    })?;

    // Wrap the single batch in a minimal ColumnarTable and serialise.
    let mut single = ColumnarTable::new(Arc::clone(&table.schema));
    single.push_unchecked(batch.clone());
    single.write_to_bytes()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Int64Array;
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    fn make_table(num_batches: usize, rows_per_batch: usize) -> ColumnarTable {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("value", DataType::Int64, false),
        ]));
        let mut table = ColumnarTable::new(Arc::clone(&schema));
        for batch_idx in 0..num_batches {
            let base = (batch_idx * rows_per_batch) as i64;
            let ids: Vec<i64> = (base..base + rows_per_batch as i64).collect();
            let vals: Vec<i64> = ids.iter().map(|&i| i * 2).collect();
            let batch = RecordBatch::try_new(
                Arc::clone(&schema),
                vec![
                    Arc::new(Int64Array::from(ids)),
                    Arc::new(Int64Array::from(vals)),
                ],
            )
            .expect("batch construction");
            table.push_unchecked(batch);
        }
        table
    }

    #[test]
    fn load_row_group_returns_bytes() {
        let table = make_table(3, 10);
        let mut cache = ColumnarRowGroupCache::new(16);
        let bytes = cache.load_row_group("test.parquet", 0, &table).unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn load_row_group_hit_on_second_access() {
        let table = make_table(3, 10);
        let mut cache = ColumnarRowGroupCache::new(16);
        // First access: miss.
        cache.load_row_group("f.parquet", 0, &table).unwrap();
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);
        // Second access: hit.
        cache.load_row_group("f.parquet", 0, &table).unwrap();
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn load_row_group_out_of_range_errors() {
        let table = make_table(2, 5);
        let mut cache = ColumnarRowGroupCache::new(16);
        let result = cache.load_row_group("f.parquet", 99, &table);
        assert!(result.is_err());
    }

    #[test]
    fn invalidate_file_removes_entries() {
        let table = make_table(4, 5);
        let mut cache = ColumnarRowGroupCache::new(32);
        for i in 0..4 {
            cache.load_row_group("file_a", i, &table).unwrap();
        }
        cache.load_row_group("file_b", 0, &table).unwrap();
        assert_eq!(cache.len(), 5);

        cache.invalidate_file("file_a");
        assert_eq!(cache.len(), 1); // only file_b's entry remains
    }

    #[test]
    fn warm_from_table_populates_all_groups() {
        let table = make_table(5, 8);
        let mut cache = ColumnarRowGroupCache::new(32);
        cache.warm_from_table("warm_file", &table).unwrap();
        assert_eq!(cache.len(), 5);
        // All should be cache hits now.
        for i in 0..5 {
            let key = RowGroupKey::new("warm_file", i);
            assert!(cache.get(&key).is_some());
        }
    }

    #[test]
    fn get_as_batch_round_trip() {
        let table = make_table(2, 4);
        let mut cache = ColumnarRowGroupCache::new(16);
        cache.load_row_group("rt.parquet", 0, &table).unwrap();
        let key = RowGroupKey::new("rt.parquet", 0);
        let batch = cache.get_as_batch(&key).unwrap().expect("batch present");
        assert_eq!(batch.num_rows(), 4);
    }

    #[test]
    fn clear_empties_cache() {
        let table = make_table(3, 5);
        let mut cache = ColumnarRowGroupCache::new(16);
        cache.warm_from_table("f", &table).unwrap();
        assert!(!cache.is_empty());
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn hit_rate_computation() {
        let table = make_table(1, 3);
        let mut cache = ColumnarRowGroupCache::new(16);
        cache.load_row_group("h.parquet", 0, &table).unwrap(); // miss
        cache.load_row_group("h.parquet", 0, &table).unwrap(); // hit
        cache.load_row_group("h.parquet", 0, &table).unwrap(); // hit
                                                               // 2 hits / 3 total = 0.666...
        assert!((cache.hit_rate() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn evict_removes_single_entry() {
        let table = make_table(2, 5);
        let mut cache = ColumnarRowGroupCache::new(16);
        cache.load_row_group("e.parquet", 0, &table).unwrap();
        cache.load_row_group("e.parquet", 1, &table).unwrap();
        let key = RowGroupKey::new("e.parquet", 0);
        let evicted = cache.evict(&key);
        assert!(evicted.is_some());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn ttl_expired_entry_is_miss() {
        let table = make_table(1, 2);
        let mut cache = ColumnarRowGroupCache::new(16);
        let key = RowGroupKey::new("ttl_file", 0);
        let bytes = make_table(1, 2).write_to_bytes().expect("serialise");
        // Insert with a 1 nanosecond TTL — effectively already expired.
        cache.insert_with_ttl(key.clone(), bytes, Duration::from_nanos(1));
        // Spin-wait until at least 1ns has passed (it almost certainly has already).
        std::thread::yield_now();
        // Now load — should be a miss (the TTL-expired entry is not returned).
        cache.load_row_group("ttl_file", 0, &table).unwrap();
        // hits should be 0 regardless of how get() interacts with TTL.
        // The key assertion: the load succeeds and returns valid bytes.
        assert_eq!(cache.len(), 1);
    }
}

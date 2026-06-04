//! SQL-layer caching: [`SqlQueryCache`] for query result sets and
//! [`SqlPlanCache`] for prepared statement plans.
//!
//! # SqlQueryCache
//!
//! Caches `RowSet` results produced by SQL queries.  Keys are normalised
//! query strings (leading/trailing whitespace collapsed, consecutive spaces
//! reduced to a single space, and ASCII letters upper-cased).  Values are
//! cloned `RowSet` instances.
//!
//! TTL support allows stale result sets to be expired automatically; the next
//! `get` after expiry returns `None` and the stale entry is removed lazily.
//!
//! # SqlPlanCache
//!
//! Stores an opaque, caller-supplied prepared-statement representation `P`
//! (e.g. a compiled query plan or a byte buffer).  Unlike [`SqlQueryCache`]
//! there is no default TTL — plans are typically valid for the lifetime of the
//! connection and are only evicted by capacity pressure or explicit removal.
//!
//! Both caches are **not** thread-safe on their own; wrap with
//! [`crate::sync::SyncCache`] or [`crate::sharded::ShardedCache`] for
//! concurrent use.

use std::time::Duration;

use oxisql_core::RowSet;

use crate::lru::LruCache;
use crate::Cache;

// ── Key normalisation ─────────────────────────────────────────────────────────

/// Normalise a SQL query string for use as a cache key.
///
/// Normalisation rules (applied in order):
/// 1. Trim leading and trailing ASCII whitespace.
/// 2. Collapse internal runs of whitespace (space, tab, newline, CR, FF) to a
///    single ASCII space `' '`.
/// 3. Convert ASCII letters to upper case.
///
/// This ensures that logically identical queries with minor formatting
/// differences share the same cache entry.
fn normalise_query(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut prev_was_space = true; // skip leading spaces
    for ch in sql.chars() {
        if ch.is_ascii_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            out.push(ch.to_ascii_uppercase());
            prev_was_space = false;
        }
    }
    // Trim trailing space that may have been appended.
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

// ── SqlQueryCache ─────────────────────────────────────────────────────────────

/// Statistics snapshot for [`SqlQueryCache`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueryCacheStats {
    /// Total number of cache hits (normalised-key lookup returned a live entry).
    pub hits: u64,
    /// Total number of cache misses (key absent or entry expired).
    pub misses: u64,
    /// Number of entries currently held in the cache.
    pub len: usize,
    /// Maximum number of entries the cache can hold.
    pub cap: usize,
}

impl QueryCacheStats {
    /// Return the hit rate as a fraction in `[0.0, 1.0]`.
    ///
    /// Returns `0.0` if no accesses have been recorded.
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// In-memory LRU cache of SQL query result sets.
///
/// Result sets are keyed by their normalised SQL text (leading/trailing whitespace collapsed,
/// consecutive spaces reduced to a single space, ASCII letters upper-cased).
/// Each entry stores a cloned [`RowSet`]; callers receive another clone on
/// `get` so the cached copy is never mutated.
///
/// # TTL
///
/// Insert with a TTL via [`SqlQueryCache::put_with_ttl`].  After the TTL
/// elapses, `get` returns `None` and the stale entry is lazily removed.
///
/// # Example
///
/// ```rust
/// use oxisql_core::{Row, RowSet, Value};
/// use oxistore_cache::sql_cache::SqlQueryCache;
///
/// let mut cache = SqlQueryCache::new(256);
/// let rows = vec![Row::new(vec!["id".into()], vec![Value::I64(1)])];
/// let rs = RowSet::from_rows(rows);
/// cache.put("SELECT id FROM t WHERE id = 1", rs.clone());
///
/// // Equivalent queries (different whitespace/case) hit the same entry.
/// assert!(cache.get("select  id  from  t  where  id = 1").is_some());
/// ```
pub struct SqlQueryCache {
    inner: LruCache<String, RowSet>,
    hits: u64,
    misses: u64,
}

impl SqlQueryCache {
    /// Create a new cache with the given maximum entry count.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: LruCache::new(capacity),
            hits: 0,
            misses: 0,
        }
    }

    /// Look up a query result set by SQL text.
    ///
    /// The query is normalised before lookup.  Returns a clone of the cached
    /// [`RowSet`] on a hit, `None` on a miss or TTL expiry.
    pub fn get(&mut self, sql: &str) -> Option<RowSet> {
        let key = normalise_query(sql);
        match self.inner.get(&key) {
            Some(rs) => {
                self.hits += 1;
                Some(rs.clone())
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    /// Insert a query result set, replacing any existing entry for the same
    /// normalised SQL.
    pub fn put(&mut self, sql: &str, result: RowSet) {
        let key = normalise_query(sql);
        self.inner.put(key, result);
    }

    /// Insert a query result set that expires after `ttl`.
    pub fn put_with_ttl(&mut self, sql: &str, result: RowSet, ttl: Duration) {
        let key = normalise_query(sql);
        self.inner.put_with_ttl(key, result, ttl);
    }

    /// Remove the cached result for `sql`, returning it if present.
    pub fn invalidate(&mut self, sql: &str) -> Option<RowSet> {
        let key = normalise_query(sql);
        self.inner.remove(&key)
    }

    /// Remove all cached results.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Return `true` if the cache contains a live entry for `sql`.
    pub fn contains(&mut self, sql: &str) -> bool {
        let key = normalise_query(sql);
        self.inner.contains_key(&key)
    }

    /// Return the number of entries currently in the cache.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Return `true` if the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Return the maximum number of entries.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.inner.cap()
    }

    /// Return a stats snapshot.
    #[must_use]
    pub fn stats(&self) -> QueryCacheStats {
        QueryCacheStats {
            hits: self.hits,
            misses: self.misses,
            len: self.inner.len(),
            cap: self.inner.cap(),
        }
    }

    /// Dynamically resize the cache capacity.
    ///
    /// If `new_cap` is smaller than the current length, entries are evicted
    /// (LRU policy) until `len() <= new_cap`.
    pub fn resize(&mut self, new_cap: usize) {
        self.inner.resize(new_cap);
    }
}

// ── SqlPlanCache ──────────────────────────────────────────────────────────────

/// In-memory LRU cache of prepared SQL statement plans.
///
/// `P` is the caller-supplied plan representation — typically a compiled query
/// plan, a byte buffer, or a `Box<dyn Any>`.  The cache is generic so it can
/// accommodate any backend's plan type without imposing a common trait.
///
/// Plans are never expired by TTL (they are considered stable for the lifetime
/// of a connection).  TTL can be added via [`SqlPlanCache::put_with_ttl`] when
/// plans have limited validity (e.g. after schema changes).
///
/// Keys are normalised SQL text (same rules as [`SqlQueryCache`]).
///
/// # Example
///
/// ```rust
/// use oxistore_cache::sql_cache::SqlPlanCache;
///
/// // Use Vec<u8> as a stand-in for a serialised plan.
/// let mut plan_cache: SqlPlanCache<Vec<u8>> = SqlPlanCache::new(512);
/// plan_cache.put("SELECT 1", vec![0x01, 0x02]);
/// assert!(plan_cache.get("select  1").is_some());
/// ```
pub struct SqlPlanCache<P> {
    inner: LruCache<String, P>,
    hits: u64,
    misses: u64,
}

impl<P: Clone> SqlPlanCache<P> {
    /// Create a new plan cache with the given maximum entry count.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: LruCache::new(capacity),
            hits: 0,
            misses: 0,
        }
    }

    /// Look up a plan by SQL text.
    ///
    /// Returns a reference to the cached plan on a hit, `None` on a miss.
    pub fn get(&mut self, sql: &str) -> Option<&P> {
        let key = normalise_query(sql);
        match self.inner.get(&key) {
            Some(p) => {
                self.hits += 1;
                Some(p)
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    /// Store a plan for `sql`.
    pub fn put(&mut self, sql: &str, plan: P) {
        let key = normalise_query(sql);
        self.inner.put(key, plan);
    }

    /// Store a plan for `sql` that expires after `ttl`.
    pub fn put_with_ttl(&mut self, sql: &str, plan: P, ttl: Duration) {
        let key = normalise_query(sql);
        self.inner.put_with_ttl(key, plan, ttl);
    }

    /// Remove the cached plan for `sql`, returning it if present.
    pub fn invalidate(&mut self, sql: &str) -> Option<P> {
        let key = normalise_query(sql);
        self.inner.remove(&key)
    }

    /// Remove all cached plans.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Return `true` if a live plan is cached for `sql`.
    pub fn contains(&mut self, sql: &str) -> bool {
        let key = normalise_query(sql);
        self.inner.contains_key(&key)
    }

    /// Return the number of plans currently in the cache.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Return `true` if the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Return the maximum number of plans.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.inner.cap()
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
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Dynamically resize the cache.
    pub fn resize(&mut self, new_cap: usize) {
        self.inner.resize(new_cap);
    }
}

// ── CachedQueryRunner ─────────────────────────────────────────────────────────

/// A read-through adapter that wraps a synchronous query executor with a
/// [`SqlQueryCache`].
///
/// The executor `F` is a `FnMut(&str) -> Result<RowSet, E>` — a closure or
/// function pointer that actually runs the SQL against a database backend.
///
/// On a cache hit the executor is never called.  On a miss the executor is
/// called, the result stored in the cache, and returned to the caller.
///
/// # Example
///
/// ```rust
/// use oxisql_core::{Row, RowSet, Value};
/// use oxistore_cache::sql_cache::CachedQueryRunner;
///
/// let mut runner = CachedQueryRunner::new(
///     32,
///     |sql: &str| -> Result<RowSet, String> {
///         // Simulated DB hit.
///         Ok(RowSet::from_rows(vec![Row::new(
///             vec!["n".into()],
///             vec![Value::I64(1)],
///         )]))
///     },
/// );
///
/// let r1 = runner.run("SELECT 1").unwrap();
/// let r2 = runner.run("SELECT 1").unwrap();
/// assert_eq!(r1.len(), r2.len());
/// assert_eq!(runner.hits(), 1);
/// assert_eq!(runner.misses(), 1);
/// ```
pub struct CachedQueryRunner<F, E>
where
    F: FnMut(&str) -> Result<RowSet, E>,
{
    cache: SqlQueryCache,
    executor: F,
}

impl<F, E> CachedQueryRunner<F, E>
where
    F: FnMut(&str) -> Result<RowSet, E>,
{
    /// Create a new runner with the given cache capacity and executor.
    pub fn new(capacity: usize, executor: F) -> Self {
        Self {
            cache: SqlQueryCache::new(capacity),
            executor,
        }
    }

    /// Run `sql` against the cache/executor.
    ///
    /// Returns a cached [`RowSet`] clone on a hit.  On a miss, calls the
    /// executor, caches the result (without TTL), and returns it.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by the executor.
    pub fn run(&mut self, sql: &str) -> Result<RowSet, E> {
        if let Some(cached) = self.cache.get(sql) {
            return Ok(cached);
        }
        let result = (self.executor)(sql)?;
        self.cache.put(sql, result.clone());
        Ok(result)
    }

    /// Run `sql` with a TTL on the cached result.
    ///
    /// Cached results expire after `ttl`; the next call after expiry will
    /// re-execute the query.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by the executor.
    pub fn run_with_ttl(&mut self, sql: &str, ttl: Duration) -> Result<RowSet, E> {
        if let Some(cached) = self.cache.get(sql) {
            return Ok(cached);
        }
        let result = (self.executor)(sql)?;
        self.cache.put_with_ttl(sql, result.clone(), ttl);
        Ok(result)
    }

    /// Invalidate a specific cached result.
    pub fn invalidate(&mut self, sql: &str) {
        self.cache.invalidate(sql);
    }

    /// Remove all cached results.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Return the number of cache hits since creation.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.cache.hits
    }

    /// Return the number of cache misses since creation.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.cache.misses
    }

    /// Return a stats snapshot.
    #[must_use]
    pub fn stats(&self) -> QueryCacheStats {
        self.cache.stats()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use oxisql_core::{Row, RowSet, Value};

    fn make_rowset(n: i64) -> RowSet {
        let rows: Vec<Row> = (0..n)
            .map(|i| Row::new(vec!["id".into()], vec![Value::I64(i)]))
            .collect();
        RowSet::from_rows(rows)
    }

    // ── normalise_query ───────────────────────────────────────────────────────

    #[test]
    fn normalise_trims_whitespace() {
        assert_eq!(normalise_query("  select 1  "), "SELECT 1");
    }

    #[test]
    fn normalise_collapses_internal_spaces() {
        assert_eq!(normalise_query("select  id   from   t"), "SELECT ID FROM T");
    }

    #[test]
    fn normalise_uppercase() {
        assert_eq!(normalise_query("select id from t"), "SELECT ID FROM T");
    }

    #[test]
    fn normalise_tabs_and_newlines() {
        assert_eq!(normalise_query("SELECT\tid\nFROM\tt"), "SELECT ID FROM T");
    }

    #[test]
    fn normalise_empty_string() {
        assert_eq!(normalise_query(""), "");
        assert_eq!(normalise_query("   "), "");
    }

    // ── SqlQueryCache ─────────────────────────────────────────────────────────

    #[test]
    fn put_and_get_basic() {
        let mut cache = SqlQueryCache::new(8);
        let rs = make_rowset(3);
        cache.put("SELECT id FROM t", rs.clone());
        let got = cache.get("SELECT id FROM t");
        assert!(got.is_some());
        assert_eq!(got.unwrap().len(), 3);
    }

    #[test]
    fn get_normalises_key() {
        let mut cache = SqlQueryCache::new(8);
        cache.put("SELECT id FROM t", make_rowset(2));
        // Different whitespace/case should hit the same entry.
        assert!(cache.get("select  id  from  t").is_some());
        assert!(cache.get("SELECT\tID\nFROM\tT").is_some());
    }

    #[test]
    fn miss_returns_none() {
        let mut cache = SqlQueryCache::new(8);
        assert!(cache.get("SELECT 1").is_none());
    }

    #[test]
    fn hits_and_misses_counted() {
        let mut cache = SqlQueryCache::new(8);
        cache.put("SELECT 1", make_rowset(1));
        cache.get("SELECT 1"); // hit
        cache.get("SELECT 2"); // miss
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert!((stats.hit_rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn invalidate_removes_entry() {
        let mut cache = SqlQueryCache::new(8);
        cache.put("SELECT 1", make_rowset(1));
        let removed = cache.invalidate("select 1"); // normalisation applies
        assert!(removed.is_some());
        assert!(cache.get("SELECT 1").is_none());
    }

    #[test]
    fn clear_empties_cache() {
        let mut cache = SqlQueryCache::new(8);
        cache.put("SELECT 1", make_rowset(1));
        cache.put("SELECT 2", make_rowset(2));
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn ttl_expiry() {
        let mut cache = SqlQueryCache::new(8);
        cache.put_with_ttl("SELECT 1", make_rowset(1), Duration::from_nanos(1));
        std::thread::yield_now();
        // After TTL, get should return None.
        assert!(cache.get("SELECT 1").is_none());
    }

    #[test]
    fn resize_evicts_excess() {
        let mut cache = SqlQueryCache::new(8);
        for i in 0..6 {
            cache.put(&format!("SELECT {i}"), make_rowset(1));
        }
        assert_eq!(cache.len(), 6);
        cache.resize(3);
        assert!(cache.len() <= 3);
    }

    #[test]
    fn cap_returns_capacity() {
        let cache = SqlQueryCache::new(100);
        assert_eq!(cache.cap(), 100);
    }

    // ── SqlPlanCache ──────────────────────────────────────────────────────────

    #[test]
    fn plan_cache_put_and_get() {
        let mut cache: SqlPlanCache<Vec<u8>> = SqlPlanCache::new(16);
        cache.put("SELECT 1", vec![0x01, 0x02, 0x03]);
        let plan = cache.get("SELECT 1").unwrap();
        assert_eq!(plan, &[0x01u8, 0x02, 0x03]);
    }

    #[test]
    fn plan_cache_normalises_key() {
        let mut cache: SqlPlanCache<Vec<u8>> = SqlPlanCache::new(16);
        cache.put("SELECT id FROM t", vec![0xAB]);
        assert!(cache.get("select  id  from  t").is_some());
    }

    #[test]
    fn plan_cache_invalidate() {
        let mut cache: SqlPlanCache<Vec<u8>> = SqlPlanCache::new(16);
        cache.put("SELECT 1", vec![1]);
        assert!(cache.invalidate("SELECT 1").is_some());
        assert!(cache.get("SELECT 1").is_none());
    }

    #[test]
    fn plan_cache_hit_miss_stats() {
        let mut cache: SqlPlanCache<Vec<u8>> = SqlPlanCache::new(16);
        cache.put("SELECT 1", vec![1]);
        cache.get("SELECT 1"); // hit
        cache.get("SELECT 2"); // miss
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
        assert!((cache.hit_rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn plan_cache_ttl_expiry() {
        let mut cache: SqlPlanCache<Vec<u8>> = SqlPlanCache::new(16);
        cache.put_with_ttl("SELECT 1", vec![1], Duration::from_nanos(1));
        std::thread::yield_now();
        assert!(cache.get("SELECT 1").is_none());
    }

    // ── CachedQueryRunner ─────────────────────────────────────────────────────

    #[test]
    fn runner_caches_result() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cc = std::sync::Arc::clone(&call_count);
        let mut runner = CachedQueryRunner::new(32, move |_sql: &str| -> Result<RowSet, String> {
            cc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(make_rowset(5))
        });
        let r1 = runner.run("SELECT id FROM t").unwrap();
        let r2 = runner.run("SELECT id FROM t").unwrap();
        assert_eq!(r1.len(), r2.len());
        // Executor was called exactly once.
        assert_eq!(call_count.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(runner.hits(), 1);
        assert_eq!(runner.misses(), 1);
    }

    #[test]
    fn runner_propagates_executor_error() {
        let mut runner = CachedQueryRunner::new(8, |_sql: &str| -> Result<RowSet, String> {
            Err("db error".to_string())
        });
        assert!(runner.run("SELECT 1").is_err());
    }

    #[test]
    fn runner_ttl_invalidates_result() {
        let mut runner = CachedQueryRunner::new(8, |_: &str| -> Result<RowSet, String> {
            Ok(make_rowset(1))
        });
        runner
            .run_with_ttl("SELECT 1", Duration::from_nanos(1))
            .unwrap();
        std::thread::yield_now();
        // After TTL expiry, the next call should re-execute.
        runner
            .run_with_ttl("SELECT 1", Duration::from_nanos(1))
            .unwrap();
        assert_eq!(runner.misses(), 2); // both calls were misses
    }

    #[test]
    fn runner_invalidate_forces_re_execution() {
        let mut runner = CachedQueryRunner::new(8, |_: &str| -> Result<RowSet, String> {
            Ok(make_rowset(1))
        });
        runner.run("SELECT 1").unwrap(); // miss
        runner.invalidate("select 1"); // evict (normalisation applies)
        runner.run("SELECT 1").unwrap(); // miss again
        assert_eq!(runner.misses(), 2);
        assert_eq!(runner.hits(), 0);
    }
}

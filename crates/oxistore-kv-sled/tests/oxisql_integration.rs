//! Integration tests verifying that the sled backend in `oxisql-embedded`
//! (`SledEmbeddedConnection` / `SledGlueStorage`) correctly persists SQL data
//! to disk and is interoperable with `oxistore-kv-sled`.
//!
//! These tests use `oxisql-embedded = "0.2.0"` with `features = ["sled-storage"]`
//! as a dev-dependency (crates.io version, no cross-workspace path coupling).
//!
//! Note: sled does not support an in-memory mode; all connections are file-backed.
//!
//! Scenarios:
//!   1. Basic CREATE TABLE / INSERT / SELECT / DELETE.
//!   2. Persistence across connection close + re-open.
//!   3. Multiple isolated tables in the same sled directory.
//!   4. Large insertion (1 000 rows) with COUNT(*) verification.
//!   5. UPDATE semantics.
//!   6. NULL value round-trip.
//!   7. DELETE all rows leaves empty table.
//!   8. Concurrent reads via cloned connections.

use std::sync::atomic::{AtomicU64, Ordering};

use oxisql_core_02::{Connection, Value};
use oxisql_embedded::SledEmbeddedConnection;

// ── helpers ──────────────────────────────────────────────────────────────────

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Return a unique temp directory path for a sled database.
fn unique_path(label: &str) -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "oxistore_sled_oxisql_{}_{}_{}_{}",
        label,
        std::process::id(),
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        },
        id,
    ))
}

// ── test 1: basic CRUD ───────────────────────────────────────────────────────

#[tokio::test]
async fn sled_oxisql_basic_crud() {
    let path = unique_path("basic_crud");
    let conn = SledEmbeddedConnection::open(&path).expect("SledEmbeddedConnection::open failed");

    conn.execute(
        "CREATE TABLE employees (id INTEGER, name TEXT, dept TEXT)",
        &[],
    )
    .await
    .expect("CREATE TABLE");

    conn.execute(
        "INSERT INTO employees VALUES (1, 'Alice', 'Engineering')",
        &[],
    )
    .await
    .expect("INSERT 1");
    conn.execute("INSERT INTO employees VALUES (2, 'Bob', 'Marketing')", &[])
        .await
        .expect("INSERT 2");
    conn.execute(
        "INSERT INTO employees VALUES (3, 'Carol', 'Engineering')",
        &[],
    )
    .await
    .expect("INSERT 3");

    let rows = conn
        .query("SELECT id, name, dept FROM employees ORDER BY id", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 3);

    let name0: String = rows[0].try_get("name").expect("name");
    let dept0: String = rows[0].try_get("dept").expect("dept");
    assert_eq!(name0, "Alice");
    assert_eq!(dept0, "Engineering");

    // Delete Bob.
    conn.execute("DELETE FROM employees WHERE id = 2", &[])
        .await
        .expect("DELETE");

    let remaining = conn
        .query("SELECT id FROM employees ORDER BY id", &[])
        .await
        .expect("SELECT after DELETE");
    assert_eq!(remaining.len(), 2);
    let ids: Vec<i64> = remaining
        .iter()
        .map(|r| r.try_get("id").expect("id"))
        .collect();
    assert_eq!(ids, vec![1, 3]);
}

// ── test 2: persistence across reconnect ────────────────────────────────────

#[tokio::test]
async fn sled_oxisql_persistence_across_reconnect() {
    let path = unique_path("persistence");

    // Write phase.
    {
        let conn = SledEmbeddedConnection::open(&path).expect("open for write");
        conn.execute("CREATE TABLE cache (key TEXT, value TEXT)", &[])
            .await
            .expect("CREATE TABLE");
        conn.execute("INSERT INTO cache VALUES ('host', 'localhost')", &[])
            .await
            .expect("INSERT host");
        conn.execute("INSERT INTO cache VALUES ('port', '5432')", &[])
            .await
            .expect("INSERT port");
    }

    // Read phase.
    {
        let conn = SledEmbeddedConnection::open(&path).expect("re-open for read");
        let rows = conn
            .query("SELECT key, value FROM cache ORDER BY key", &[])
            .await
            .expect("SELECT after reconnect");
        assert_eq!(rows.len(), 2, "expected 2 persisted rows");
        // ORDER BY key: 'host' < 'port'
        let key0: String = rows[0].try_get("key").expect("key");
        let val0: String = rows[0].try_get("value").expect("value");
        assert_eq!(key0, "host");
        assert_eq!(val0, "localhost");

        let key1: String = rows[1].try_get("key").expect("key");
        let val1: String = rows[1].try_get("value").expect("value");
        assert_eq!(key1, "port");
        assert_eq!(val1, "5432");
    }
}

// ── test 3: multiple table isolation ────────────────────────────────────────

#[tokio::test]
async fn sled_oxisql_multiple_table_isolation() {
    let path = unique_path("multi_table");
    let conn = SledEmbeddedConnection::open(&path).expect("open");

    conn.execute("CREATE TABLE table_a (n INTEGER)", &[])
        .await
        .expect("CREATE a");
    conn.execute("CREATE TABLE table_b (n INTEGER)", &[])
        .await
        .expect("CREATE b");
    conn.execute("INSERT INTO table_a VALUES (100)", &[])
        .await
        .expect("INSERT a");
    conn.execute("INSERT INTO table_b VALUES (200)", &[])
        .await
        .expect("INSERT b");

    let ra = conn
        .query("SELECT n FROM table_a", &[])
        .await
        .expect("SELECT a");
    assert_eq!(ra.len(), 1);
    let na: i64 = ra[0].try_get("n").expect("n");
    assert_eq!(na, 100, "table_a contains table_b's data");

    let rb = conn
        .query("SELECT n FROM table_b", &[])
        .await
        .expect("SELECT b");
    assert_eq!(rb.len(), 1);
    let nb: i64 = rb[0].try_get("n").expect("n");
    assert_eq!(nb, 200, "table_b contains table_a's data");
}

// ── test 4: large insertion ──────────────────────────────────────────────────

#[tokio::test]
async fn sled_oxisql_large_row_count() {
    let path = unique_path("large");
    let conn = SledEmbeddedConnection::open(&path).expect("open");

    conn.execute("CREATE TABLE bulk (id INTEGER, label TEXT)", &[])
        .await
        .expect("CREATE TABLE");

    for i in 0i64..1_000 {
        let sql = format!("INSERT INTO bulk VALUES ({}, 'label_{}')", i, i);
        conn.execute(&sql, &[])
            .await
            .unwrap_or_else(|e| panic!("INSERT {} failed: {}", i, e));
    }

    let cnt_rows = conn
        .query("SELECT COUNT(*) as cnt FROM bulk", &[])
        .await
        .expect("COUNT(*)");
    assert_eq!(cnt_rows.len(), 1);
    let cnt: i64 = cnt_rows[0].try_get("cnt").expect("cnt");
    assert_eq!(cnt, 1_000, "expected 1000 rows, got {}", cnt);
}

// ── test 5: UPDATE semantics ─────────────────────────────────────────────────

#[tokio::test]
async fn sled_oxisql_update_row() {
    let path = unique_path("update");
    let conn = SledEmbeddedConnection::open(&path).expect("open");

    conn.execute("CREATE TABLE settings (name TEXT, val INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO settings VALUES ('timeout', 30)", &[])
        .await
        .expect("INSERT");

    conn.execute("UPDATE settings SET val = 60 WHERE name = 'timeout'", &[])
        .await
        .expect("UPDATE");

    let rows = conn
        .query("SELECT val FROM settings WHERE name = 'timeout'", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);
    let val: i64 = rows[0].try_get("val").expect("val");
    assert_eq!(val, 60, "expected updated value 60");
}

// ── test 6: NULL round-trip ──────────────────────────────────────────────────

#[tokio::test]
async fn sled_oxisql_null_round_trip() {
    let path = unique_path("null");
    let conn = SledEmbeddedConnection::open(&path).expect("open");

    conn.execute("CREATE TABLE nullable (id INTEGER, note TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO nullable VALUES (1, NULL)", &[])
        .await
        .expect("INSERT NULL");
    conn.execute("INSERT INTO nullable VALUES (2, 'non-null')", &[])
        .await
        .expect("INSERT non-NULL");

    let rows = conn
        .query("SELECT id, note FROM nullable ORDER BY id", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 2);

    let note_val = rows[0].get("note").expect("note column");
    assert!(
        matches!(note_val, Value::Null),
        "expected Null for row 1, got {:?}",
        note_val
    );

    let note2: String = rows[1].try_get("note").expect("note");
    assert_eq!(note2, "non-null");
}

// ── test 7: DELETE all rows ──────────────────────────────────────────────────

#[tokio::test]
async fn sled_oxisql_delete_all_rows() {
    let path = unique_path("delete_all");
    let conn = SledEmbeddedConnection::open(&path).expect("open");

    conn.execute("CREATE TABLE queue (msg TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO queue VALUES ('msg1')", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO queue VALUES ('msg2')", &[])
        .await
        .expect("INSERT 2");

    let before = conn
        .query("SELECT msg FROM queue", &[])
        .await
        .expect("SELECT before");
    assert_eq!(before.len(), 2);

    conn.execute("DELETE FROM queue", &[])
        .await
        .expect("DELETE all");

    let after = conn
        .query("SELECT msg FROM queue", &[])
        .await
        .expect("SELECT after");
    assert_eq!(after.len(), 0, "expected empty table after full delete");
}

// ── test 8: concurrent reads ─────────────────────────────────────────────────

#[tokio::test]
async fn sled_oxisql_concurrent_reads() {
    let path = unique_path("concurrent");
    let conn = SledEmbeddedConnection::open(&path).expect("open");

    conn.execute("CREATE TABLE readings (sensor TEXT, temp INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO readings VALUES ('A', 22)", &[])
        .await
        .expect("INSERT A");
    conn.execute("INSERT INTO readings VALUES ('B', 25)", &[])
        .await
        .expect("INSERT B");

    let c2 = conn.clone();
    let c3 = conn.clone();

    let (r2, r3) = tokio::join!(
        c2.query("SELECT sensor, temp FROM readings ORDER BY sensor", &[]),
        c3.query("SELECT sensor, temp FROM readings ORDER BY sensor", &[]),
    );

    let r2 = r2.expect("reader2 SELECT");
    let r3 = r3.expect("reader3 SELECT");

    assert_eq!(r2.len(), 2, "reader2: expected 2 rows");
    assert_eq!(r3.len(), 2, "reader3: expected 2 rows");

    let sensor2_0: String = r2[0].try_get("sensor").expect("sensor");
    let sensor3_0: String = r3[0].try_get("sensor").expect("sensor");
    assert_eq!(sensor2_0, "A");
    assert_eq!(sensor3_0, "A");
}

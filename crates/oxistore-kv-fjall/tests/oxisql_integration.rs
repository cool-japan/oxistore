//! Integration tests verifying that the fjall backend in `oxisql-embedded`
//! (`FjallEmbeddedConnection` / `FjallGlueStorage`) correctly persists SQL
//! data to disk and that it is interoperable with `oxistore-kv-fjall`.
//!
//! These tests use `oxisql-embedded = "0.2.0"` with `features = ["fjall-storage"]`
//! as a dev-dependency (crates.io, no cross-workspace path coupling).
//!
//! The key scenarios verified:
//!   1. Basic CREATE TABLE / INSERT / SELECT / DELETE via FjallEmbeddedConnection.
//!   2. Persistence — data survives connection close and re-open to the same path.
//!   3. Multiple tables in the same fjall database are isolated.
//!   4. Large row insertion (1 000 rows) and exact count verification.
//!   5. Concurrent read connections share data correctly.
//!   6. DELETE removes rows; subsequent SELECT returns empty result.

use std::sync::atomic::{AtomicU64, Ordering};

use oxisql_core_02::{Connection, Value};
use oxisql_embedded::FjallEmbeddedConnection;

// ── helpers ──────────────────────────────────────────────────────────────────

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Return a unique temp-dir path for a test, cleaned up by the OS on next boot.
///
/// Each call produces a distinct path, so parallel tests don't collide.
fn unique_path(label: &str) -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "oxistore_fjall_oxisql_{}_{}_{}_{}",
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

/// Verify basic CREATE TABLE / INSERT / SELECT / DELETE through the fjall backend.
#[tokio::test]
async fn fjall_oxisql_basic_crud() {
    let path = unique_path("basic_crud");
    let conn = FjallEmbeddedConnection::open(&path).expect("FjallEmbeddedConnection::open failed");

    // Create table.
    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[])
        .await
        .expect("CREATE TABLE failed");

    // Insert two rows.
    conn.execute("INSERT INTO users VALUES (1, 'Alice')", &[])
        .await
        .expect("INSERT 1 failed");
    conn.execute("INSERT INTO users VALUES (2, 'Bob')", &[])
        .await
        .expect("INSERT 2 failed");

    // Select and verify.
    let rows = conn
        .query("SELECT id, name FROM users ORDER BY id", &[])
        .await
        .expect("SELECT failed");
    assert_eq!(rows.len(), 2, "expected 2 rows, got {}", rows.len());

    let id1: i64 = rows[0].try_get("id").expect("id column missing");
    let name1: String = rows[0].try_get("name").expect("name column missing");
    assert_eq!(id1, 1);
    assert_eq!(name1, "Alice");

    let id2: i64 = rows[1].try_get("id").expect("id column missing");
    let name2: String = rows[1].try_get("name").expect("name column missing");
    assert_eq!(id2, 2);
    assert_eq!(name2, "Bob");

    // Delete one row.
    conn.execute("DELETE FROM users WHERE id = 1", &[])
        .await
        .expect("DELETE failed");

    let remaining = conn
        .query("SELECT id FROM users", &[])
        .await
        .expect("SELECT after DELETE failed");
    assert_eq!(remaining.len(), 1, "expected 1 row after delete");
    let rid: i64 = remaining[0].try_get("id").expect("id column missing");
    assert_eq!(rid, 2);
}

// ── test 2: persistence across reconnect ────────────────────────────────────

/// Verify that data written through one connection is readable after re-opening
/// the same path with a new `FjallEmbeddedConnection`.
#[tokio::test]
async fn fjall_oxisql_persistence_across_reconnect() {
    let path = unique_path("persistence");

    // Write phase.
    {
        let conn = FjallEmbeddedConnection::open(&path).expect("open for write failed");
        conn.execute("CREATE TABLE items (sku TEXT, qty INTEGER)", &[])
            .await
            .expect("CREATE TABLE failed");
        conn.execute("INSERT INTO items VALUES ('SKU-001', 100)", &[])
            .await
            .expect("INSERT failed");
        conn.execute("INSERT INTO items VALUES ('SKU-002', 250)", &[])
            .await
            .expect("INSERT failed");
        // `conn` dropped here — all data should be flushed to fjall.
    }

    // Read phase — new connection, same path.
    {
        let conn = FjallEmbeddedConnection::open(&path).expect("re-open for read failed");
        let rows = conn
            .query("SELECT sku, qty FROM items ORDER BY sku", &[])
            .await
            .expect("SELECT after reconnect failed");
        assert_eq!(
            rows.len(),
            2,
            "expected 2 persisted rows, got {}",
            rows.len()
        );

        let sku1: String = rows[0].try_get("sku").expect("sku missing");
        let qty1: i64 = rows[0].try_get("qty").expect("qty missing");
        assert_eq!(sku1, "SKU-001");
        assert_eq!(qty1, 100);

        let sku2: String = rows[1].try_get("sku").expect("sku missing");
        let qty2: i64 = rows[1].try_get("qty").expect("qty missing");
        assert_eq!(sku2, "SKU-002");
        assert_eq!(qty2, 250);
    }
}

// ── test 3: multiple table isolation ────────────────────────────────────────

/// Two tables in the same fjall database should be fully isolated.
#[tokio::test]
async fn fjall_oxisql_multiple_table_isolation() {
    let path = unique_path("multi_table");
    let conn = FjallEmbeddedConnection::open(&path).expect("open failed");

    conn.execute("CREATE TABLE alpha (val TEXT)", &[])
        .await
        .expect("CREATE alpha failed");
    conn.execute("CREATE TABLE beta  (val TEXT)", &[])
        .await
        .expect("CREATE beta failed");

    conn.execute("INSERT INTO alpha VALUES ('from_alpha')", &[])
        .await
        .expect("INSERT alpha failed");
    conn.execute("INSERT INTO beta  VALUES ('from_beta')", &[])
        .await
        .expect("INSERT beta failed");

    let alpha_rows = conn
        .query("SELECT val FROM alpha", &[])
        .await
        .expect("SELECT alpha failed");
    assert_eq!(alpha_rows.len(), 1);
    let av: String = alpha_rows[0].try_get("val").expect("val missing");
    assert_eq!(av, "from_alpha", "alpha table polluted by beta");

    let beta_rows = conn
        .query("SELECT val FROM beta", &[])
        .await
        .expect("SELECT beta failed");
    assert_eq!(beta_rows.len(), 1);
    let bv: String = beta_rows[0].try_get("val").expect("val missing");
    assert_eq!(bv, "from_beta", "beta table polluted by alpha");
}

// ── test 4: large insertion count ───────────────────────────────────────────

/// Insert 1 000 rows and verify exact count via SELECT COUNT(*).
#[tokio::test]
async fn fjall_oxisql_large_row_count() {
    let path = unique_path("large");
    let conn = FjallEmbeddedConnection::open(&path).expect("open failed");

    conn.execute("CREATE TABLE metrics (id INTEGER, value TEXT)", &[])
        .await
        .expect("CREATE TABLE failed");

    for i in 0u32..1_000 {
        let sql = format!("INSERT INTO metrics VALUES ({}, 'v{}')", i, i);
        conn.execute(&sql, &[])
            .await
            .unwrap_or_else(|e| panic!("INSERT {} failed: {}", i, e));
    }

    let rows = conn
        .query("SELECT COUNT(*) as cnt FROM metrics", &[])
        .await
        .expect("COUNT(*) failed");
    assert_eq!(rows.len(), 1, "COUNT(*) should return 1 row");
    let cnt: i64 = rows[0].try_get("cnt").expect("cnt missing");
    assert_eq!(cnt, 1_000, "expected 1000 rows, got {}", cnt);
}

// ── test 5: concurrent read connections ─────────────────────────────────────

/// Two cloned connections should see the same committed data.
#[tokio::test]
async fn fjall_oxisql_concurrent_reads() {
    let path = unique_path("concurrent");
    let conn = FjallEmbeddedConnection::open(&path).expect("open failed");

    conn.execute("CREATE TABLE events (ts INTEGER, msg TEXT)", &[])
        .await
        .expect("CREATE TABLE failed");
    conn.execute("INSERT INTO events VALUES (1000, 'start')", &[])
        .await
        .expect("INSERT failed");
    conn.execute("INSERT INTO events VALUES (2000, 'stop')", &[])
        .await
        .expect("INSERT failed");

    // Two clones reading independently must see 2 rows each.
    let conn2 = conn.clone();
    let conn3 = conn.clone();

    let (rows2, rows3) = tokio::join!(
        conn2.query("SELECT ts FROM events ORDER BY ts", &[]),
        conn3.query("SELECT ts FROM events ORDER BY ts", &[]),
    );

    let rows2 = rows2.expect("reader2 SELECT failed");
    let rows3 = rows3.expect("reader3 SELECT failed");

    assert_eq!(rows2.len(), 2, "reader2: expected 2 rows");
    assert_eq!(rows3.len(), 2, "reader3: expected 2 rows");

    let ts2_0: i64 = rows2[0].try_get("ts").expect("ts missing");
    let ts3_0: i64 = rows3[0].try_get("ts").expect("ts missing");
    assert_eq!(ts2_0, 1000);
    assert_eq!(ts3_0, 1000);
}

// ── test 6: delete clears all rows ──────────────────────────────────────────

/// After DELETE FROM without WHERE, SELECT returns an empty result set.
#[tokio::test]
async fn fjall_oxisql_delete_all_rows() {
    let path = unique_path("delete_all");
    let conn = FjallEmbeddedConnection::open(&path).expect("open failed");

    conn.execute("CREATE TABLE log (msg TEXT)", &[])
        .await
        .expect("CREATE TABLE failed");
    conn.execute("INSERT INTO log VALUES ('entry1')", &[])
        .await
        .expect("INSERT failed");
    conn.execute("INSERT INTO log VALUES ('entry2')", &[])
        .await
        .expect("INSERT failed");

    // Verify rows exist.
    let before = conn
        .query("SELECT msg FROM log", &[])
        .await
        .expect("SELECT before DELETE failed");
    assert_eq!(before.len(), 2);

    // Delete all.
    conn.execute("DELETE FROM log", &[])
        .await
        .expect("DELETE FROM failed");

    let after = conn
        .query("SELECT msg FROM log", &[])
        .await
        .expect("SELECT after DELETE failed");
    assert_eq!(after.len(), 0, "expected empty table after DELETE FROM log");
}

// ── test 7: UPDATE modifies rows ────────────────────────────────────────────

/// UPDATE a specific row and verify only that row changed.
#[tokio::test]
async fn fjall_oxisql_update_row() {
    let path = unique_path("update");
    let conn = FjallEmbeddedConnection::open(&path).expect("open failed");

    conn.execute("CREATE TABLE scores (player TEXT, score INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");
    conn.execute("INSERT INTO scores VALUES ('Alice', 50)", &[])
        .await
        .expect("INSERT Alice failed");
    conn.execute("INSERT INTO scores VALUES ('Bob', 80)", &[])
        .await
        .expect("INSERT Bob failed");

    // Update Alice's score.
    conn.execute("UPDATE scores SET score = 95 WHERE player = 'Alice'", &[])
        .await
        .expect("UPDATE failed");

    let rows = conn
        .query("SELECT player, score FROM scores ORDER BY player", &[])
        .await
        .expect("SELECT after UPDATE failed");
    assert_eq!(rows.len(), 2);

    // Alice should be first alphabetically.
    let alice_score: i64 = rows[0].try_get("score").expect("score missing");
    assert_eq!(alice_score, 95, "Alice score should be 95 after UPDATE");

    let bob_score: i64 = rows[1].try_get("score").expect("score missing");
    assert_eq!(bob_score, 80, "Bob score should be unchanged at 80");
}

// ── test 8: NULL values are stored and retrieved correctly ───────────────────

/// INSERT a NULL value; SELECT verifies it comes back as Value::Null.
#[tokio::test]
async fn fjall_oxisql_null_values() {
    let path = unique_path("null");
    let conn = FjallEmbeddedConnection::open(&path).expect("open failed");

    conn.execute("CREATE TABLE nullable (id INTEGER, comment TEXT)", &[])
        .await
        .expect("CREATE TABLE failed");
    conn.execute("INSERT INTO nullable VALUES (1, NULL)", &[])
        .await
        .expect("INSERT NULL failed");
    conn.execute("INSERT INTO nullable VALUES (2, 'present')", &[])
        .await
        .expect("INSERT present failed");

    let rows = conn
        .query("SELECT id, comment FROM nullable ORDER BY id", &[])
        .await
        .expect("SELECT failed");
    assert_eq!(rows.len(), 2);

    // Row with NULL comment.
    let comment_val = rows[0].get("comment").expect("comment column missing");
    assert!(
        matches!(comment_val, Value::Null),
        "expected Null for comment in row 1, got {:?}",
        comment_val
    );

    // Row with non-NULL comment.
    let comment_present: String = rows[1].try_get("comment").expect("comment missing");
    assert_eq!(comment_present, "present");
}

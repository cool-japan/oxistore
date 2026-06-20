//! Integration tests verifying that the redb backend in `oxisql-embedded`
//! (`RedbEmbeddedConnection` / `RedbGlueStorage`) correctly persists SQL data
//! to disk and that it is interoperable with `oxistore-kv-redb`.
//!
//! These tests use `oxisql-embedded = "0.2.0"` with `features = ["redb-storage"]`
//! as a dev-dependency (crates.io version, no cross-workspace path coupling).
//!
//! Scenarios:
//!   1. Basic CREATE TABLE / INSERT / SELECT / DELETE.
//!   2. Persistence across connection close + re-open to the same file.
//!   3. Multiple tables isolated within the same redb database file.
//!   4. In-memory (`open_in_memory`) for purely ephemeral tests.
//!   5. Large insertion (1 000 rows) with COUNT(*) verification.
//!   6. UPDATE semantics and ORDER BY correctness.
//!   7. NULL value round-trip.
//!   8. Concurrent reads via cloned connections.

use std::sync::atomic::{AtomicU64, Ordering};

use oxisql_core_02::{Connection, Value};
use oxisql_embedded::RedbEmbeddedConnection;

// ── helpers ──────────────────────────────────────────────────────────────────

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Return a unique temp-file path for a redb database.
fn unique_path(label: &str) -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "oxistore_redb_oxisql_{}_{}_{}_{}.redb",
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

/// Basic CREATE TABLE / INSERT / SELECT / DELETE via the redb backend.
#[tokio::test]
async fn redb_oxisql_basic_crud() {
    let path = unique_path("basic_crud");
    let conn = RedbEmbeddedConnection::open(&path).expect("RedbEmbeddedConnection::open failed");

    conn.execute(
        "CREATE TABLE products (id INTEGER, name TEXT, price INTEGER)",
        &[],
    )
    .await
    .expect("CREATE TABLE failed");

    conn.execute("INSERT INTO products VALUES (1, 'Widget', 999)", &[])
        .await
        .expect("INSERT 1 failed");
    conn.execute("INSERT INTO products VALUES (2, 'Gadget', 1499)", &[])
        .await
        .expect("INSERT 2 failed");
    conn.execute("INSERT INTO products VALUES (3, 'Doohickey', 299)", &[])
        .await
        .expect("INSERT 3 failed");

    let rows = conn
        .query("SELECT id, name, price FROM products ORDER BY id", &[])
        .await
        .expect("SELECT failed");
    assert_eq!(rows.len(), 3);

    let id1: i64 = rows[0].try_get("id").expect("id");
    let name1: String = rows[0].try_get("name").expect("name");
    let price1: i64 = rows[0].try_get("price").expect("price");
    assert_eq!((id1, name1.as_str(), price1), (1, "Widget", 999));

    // Delete middle row.
    conn.execute("DELETE FROM products WHERE id = 2", &[])
        .await
        .expect("DELETE failed");

    let after = conn
        .query("SELECT id FROM products ORDER BY id", &[])
        .await
        .expect("SELECT after DELETE");
    assert_eq!(after.len(), 2);
    let remaining_ids: Vec<i64> = after.iter().map(|r| r.try_get("id").expect("id")).collect();
    assert_eq!(remaining_ids, vec![1, 3]);
}

// ── test 2: persistence across reconnect ────────────────────────────────────

/// Data written with one `RedbEmbeddedConnection` must survive a re-open.
#[tokio::test]
async fn redb_oxisql_persistence_across_reconnect() {
    let path = unique_path("persistence");

    // Write phase.
    {
        let conn = RedbEmbeddedConnection::open(&path).expect("open for write failed");
        conn.execute("CREATE TABLE sessions (token TEXT, uid INTEGER)", &[])
            .await
            .expect("CREATE TABLE");
        conn.execute("INSERT INTO sessions VALUES ('abc123', 42)", &[])
            .await
            .expect("INSERT 1");
        conn.execute("INSERT INTO sessions VALUES ('xyz789', 7)", &[])
            .await
            .expect("INSERT 2");
    }

    // Read phase — new connection.
    {
        let conn = RedbEmbeddedConnection::open(&path).expect("re-open failed");
        let rows = conn
            .query("SELECT token, uid FROM sessions ORDER BY uid", &[])
            .await
            .expect("SELECT after reconnect");
        assert_eq!(rows.len(), 2, "expected 2 persisted rows");
        let uid0: i64 = rows[0].try_get("uid").expect("uid");
        let uid1: i64 = rows[1].try_get("uid").expect("uid");
        assert_eq!(uid0, 7);
        assert_eq!(uid1, 42);
    }
}

// ── test 3: in-memory is ephemeral ──────────────────────────────────────────

/// `open_in_memory` provides a fully functional but non-persistent connection.
#[tokio::test]
async fn redb_oxisql_in_memory_ephemeral() {
    let conn = RedbEmbeddedConnection::open_in_memory().expect("open_in_memory failed");

    conn.execute("CREATE TABLE tmp (n INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO tmp VALUES (99)", &[])
        .await
        .expect("INSERT");

    let rows = conn.query("SELECT n FROM tmp", &[]).await.expect("SELECT");
    assert_eq!(rows.len(), 1);
    let n: i64 = rows[0].try_get("n").expect("n");
    assert_eq!(n, 99);
}

// ── test 4: multiple table isolation ────────────────────────────────────────

/// Two tables in the same redb file must not leak rows into each other.
#[tokio::test]
async fn redb_oxisql_multiple_table_isolation() {
    let path = unique_path("multi_table");
    let conn = RedbEmbeddedConnection::open(&path).expect("open failed");

    conn.execute("CREATE TABLE cats (name TEXT)", &[])
        .await
        .expect("CREATE cats");
    conn.execute("CREATE TABLE dogs (name TEXT)", &[])
        .await
        .expect("CREATE dogs");
    conn.execute("INSERT INTO cats VALUES ('Mittens')", &[])
        .await
        .expect("INSERT cat");
    conn.execute("INSERT INTO dogs VALUES ('Rex')", &[])
        .await
        .expect("INSERT dog");

    let cats = conn
        .query("SELECT name FROM cats", &[])
        .await
        .expect("SELECT cats");
    assert_eq!(cats.len(), 1);
    let cat_name: String = cats[0].try_get("name").expect("name");
    assert_eq!(cat_name, "Mittens");

    let dogs = conn
        .query("SELECT name FROM dogs", &[])
        .await
        .expect("SELECT dogs");
    assert_eq!(dogs.len(), 1);
    let dog_name: String = dogs[0].try_get("name").expect("name");
    assert_eq!(dog_name, "Rex");
}

// ── test 5: large insertion ──────────────────────────────────────────────────

/// Insert 1 000 rows; COUNT(*) must return exactly 1 000.
#[tokio::test]
async fn redb_oxisql_large_row_count() {
    let conn = RedbEmbeddedConnection::open_in_memory().expect("open_in_memory failed");

    conn.execute("CREATE TABLE data (id INTEGER, payload TEXT)", &[])
        .await
        .expect("CREATE TABLE");

    for i in 0i64..1_000 {
        let sql = format!("INSERT INTO data VALUES ({}, 'payload_{}')", i, i);
        conn.execute(&sql, &[])
            .await
            .unwrap_or_else(|e| panic!("INSERT {} failed: {}", i, e));
    }

    let count_rows = conn
        .query("SELECT COUNT(*) as cnt FROM data", &[])
        .await
        .expect("COUNT(*)");
    assert_eq!(count_rows.len(), 1);
    let cnt: i64 = count_rows[0].try_get("cnt").expect("cnt");
    assert_eq!(cnt, 1_000);
}

// ── test 6: UPDATE semantics ─────────────────────────────────────────────────

/// UPDATE a row and verify the change is reflected in subsequent SELECT.
#[tokio::test]
async fn redb_oxisql_update_row() {
    let conn = RedbEmbeddedConnection::open_in_memory().expect("open_in_memory failed");

    conn.execute("CREATE TABLE kv (k TEXT, v INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO kv VALUES ('counter', 0)", &[])
        .await
        .expect("INSERT");

    conn.execute("UPDATE kv SET v = 42 WHERE k = 'counter'", &[])
        .await
        .expect("UPDATE");

    let rows = conn
        .query("SELECT v FROM kv WHERE k = 'counter'", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);
    let v: i64 = rows[0].try_get("v").expect("v");
    assert_eq!(v, 42);
}

// ── test 7: NULL round-trip ──────────────────────────────────────────────────

/// NULL values survive INSERT → SELECT without coercion.
#[tokio::test]
async fn redb_oxisql_null_round_trip() {
    let conn = RedbEmbeddedConnection::open_in_memory().expect("open_in_memory failed");

    conn.execute("CREATE TABLE nullable (id INTEGER, note TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO nullable VALUES (1, NULL)", &[])
        .await
        .expect("INSERT NULL");
    conn.execute("INSERT INTO nullable VALUES (2, 'something')", &[])
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
        "expected Null, got {:?}",
        note_val
    );

    let note_str: String = rows[1].try_get("note").expect("note");
    assert_eq!(note_str, "something");
}

// ── test 8: concurrent reads ─────────────────────────────────────────────────

/// Cloned connections all see the same committed data.
#[tokio::test]
async fn redb_oxisql_concurrent_reads() {
    let conn = RedbEmbeddedConnection::open_in_memory().expect("open_in_memory failed");

    conn.execute("CREATE TABLE shared (val INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO shared VALUES (10)", &[])
        .await
        .expect("INSERT 10");
    conn.execute("INSERT INTO shared VALUES (20)", &[])
        .await
        .expect("INSERT 20");
    conn.execute("INSERT INTO shared VALUES (30)", &[])
        .await
        .expect("INSERT 30");

    let c2 = conn.clone();
    let c3 = conn.clone();

    let (r2, r3) = tokio::join!(
        c2.query("SELECT val FROM shared ORDER BY val", &[]),
        c3.query("SELECT val FROM shared ORDER BY val", &[]),
    );

    let r2 = r2.expect("reader2");
    let r3 = r3.expect("reader3");

    assert_eq!(r2.len(), 3, "reader2 expected 3 rows");
    assert_eq!(r3.len(), 3, "reader3 expected 3 rows");

    let vals2: Vec<i64> = r2.iter().map(|r| r.try_get("val").expect("val")).collect();
    let vals3: Vec<i64> = r3.iter().map(|r| r.try_get("val").expect("val")).collect();
    assert_eq!(vals2, vec![10, 20, 30]);
    assert_eq!(vals3, vec![10, 20, 30]);
}

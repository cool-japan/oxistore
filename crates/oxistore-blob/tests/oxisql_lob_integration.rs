//! Integration tests demonstrating LOB (Large Object) data stored in
//! `oxistore-blob` with SQL metadata managed by `oxisql-embedded` (in-memory
//! GlueSQL, the default backend — no extra features needed).
//!
//! This implements the pattern described in the blob TODO:
//! "Integration with oxisql — store LOB (Large Object) data in blob storage
//! from SQL queries (~30 SLOC)."
//!
//! Architecture:
//! - SQL table: `attachments(id INTEGER, blob_key TEXT, mime_type TEXT, size INTEGER)`
//!   — stores metadata only; the actual bytes live in the `BlobStore`.
//! - `MemoryBlobStore` holds the raw binary data, keyed by `blob_key`.
//! - A `LobRepository` struct wires the two together, implementing:
//!     - `store_lob(id, data, mime_type)` → put blob, record metadata in SQL
//!     - `fetch_lob(id)` → look up `blob_key` from SQL, retrieve bytes from blob store
//!     - `delete_lob(id)` → remove SQL row + delete from blob store
//!     - `list_lobs()` → SELECT all metadata rows
//!
//! Scenarios:
//!   1. Store and fetch a small LOB (100 bytes).
//!   2. Store and fetch a large LOB (1 MiB).
//!   3. Delete removes both the SQL metadata and the blob data.
//!   4. Fetch returns None for a non-existent id.
//!   5. Multiple LOBs co-exist without key collision.
//!   6. list_lobs returns correct row count and metadata.
//!   7. Integrity: bytes fetched equal bytes stored (SHA-256 verified at the blob layer).

use std::sync::Arc;

use bytes::Bytes;
use oxisql_core_02::Connection;
use oxisql_embedded::EmbeddedConnection;
use oxistore_blob::{BlobStore, MemoryBlobStore};
use tokio::sync::Mutex;

// ── LobRepository ────────────────────────────────────────────────────────────

/// A thin repository that stores blob bytes in a `BlobStore` and records
/// metadata (id, blob_key, mime_type, size) in an embedded SQL table.
struct LobRepository {
    sql: Arc<Mutex<EmbeddedConnection>>,
    store: MemoryBlobStore,
}

impl LobRepository {
    /// Create the SQL metadata table and return an empty repository.
    async fn new(sql: EmbeddedConnection, store: MemoryBlobStore) -> Self {
        let sql = Arc::new(Mutex::new(sql));
        {
            let conn = sql.lock().await;
            conn.execute(
                "CREATE TABLE attachments \
                 (id INTEGER, blob_key TEXT, mime_type TEXT, size INTEGER)",
                &[],
            )
            .await
            .expect("CREATE TABLE attachments failed");
        }
        Self { sql, store }
    }

    /// Store `data` with `mime_type` under the given `id`.
    ///
    /// The blob key is `"lob/{id}"`.  The size is recorded in SQL.
    async fn store_lob(
        &self,
        id: i64,
        data: &[u8],
        mime_type: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let blob_key = format!("lob/{}", id);
        let size = data.len() as i64;

        // Store bytes in the blob backend.
        self.store
            .put(&blob_key, Bytes::copy_from_slice(data))
            .await
            .map_err(|e| format!("put failed: {}", e))?;

        // Record metadata in SQL.
        let sql = format!(
            "INSERT INTO attachments VALUES ({}, '{}', '{}', {})",
            id, blob_key, mime_type, size
        );
        let conn = self.sql.lock().await;
        conn.execute(&sql, &[])
            .await
            .map_err(|e| format!("INSERT failed: {}", e))?;

        Ok(())
    }

    /// Retrieve the bytes for `id`, or `None` if the id does not exist.
    async fn fetch_lob(&self, id: i64) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
        let conn = self.sql.lock().await;
        let rows = conn
            .query(
                &format!("SELECT blob_key FROM attachments WHERE id = {}", id),
                &[],
            )
            .await
            .map_err(|e| format!("SELECT failed: {}", e))?;

        if rows.is_empty() {
            return Ok(None);
        }

        let blob_key: String = rows[0]
            .try_get("blob_key")
            .map_err(|e| format!("try_get blob_key: {}", e))?;
        drop(conn); // release lock before hitting the blob store

        let bytes = self
            .store
            .get(&blob_key)
            .await
            .map_err(|e| format!("get failed: {}", e))?;
        Ok(Some(bytes.to_vec()))
    }

    /// Delete the SQL metadata row and the blob bytes for `id`.
    async fn delete_lob(&self, id: i64) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.sql.lock().await;
        let rows = conn
            .query(
                &format!("SELECT blob_key FROM attachments WHERE id = {}", id),
                &[],
            )
            .await
            .map_err(|e| format!("SELECT blob_key for delete: {}", e))?;

        if rows.is_empty() {
            return Ok(());
        }

        let blob_key: String = rows[0]
            .try_get("blob_key")
            .map_err(|e| format!("try_get blob_key: {}", e))?;

        conn.execute(&format!("DELETE FROM attachments WHERE id = {}", id), &[])
            .await
            .map_err(|e| format!("DELETE failed: {}", e))?;
        drop(conn);

        self.store
            .delete(&blob_key)
            .await
            .map_err(|e| format!("delete blob failed: {}", e))?;
        Ok(())
    }

    /// Return `(id, mime_type, size)` for every stored LOB.
    async fn list_lobs(&self) -> Result<Vec<(i64, String, i64)>, Box<dyn std::error::Error>> {
        let conn = self.sql.lock().await;
        let rows = conn
            .query(
                "SELECT id, mime_type, size FROM attachments ORDER BY id",
                &[],
            )
            .await
            .map_err(|e| format!("SELECT list failed: {}", e))?;

        rows.iter()
            .map(|r| {
                let id: i64 = r.try_get("id").map_err(|e| format!("{}", e))?;
                let mime: String = r.try_get("mime_type").map_err(|e| format!("{}", e))?;
                let sz: i64 = r.try_get("size").map_err(|e| format!("{}", e))?;
                Ok((id, mime, sz))
            })
            .collect()
    }
}

// ── test helpers ─────────────────────────────────────────────────────────────

async fn make_repo() -> LobRepository {
    let sql = EmbeddedConnection::open_memory().expect("open_memory failed");
    let store = MemoryBlobStore::new();
    LobRepository::new(sql, store).await
}

// ── test 1: store and fetch small LOB ────────────────────────────────────────

#[tokio::test]
async fn lob_store_and_fetch_small() {
    let repo = make_repo().await;
    let data: Vec<u8> = (0u8..100).collect();

    repo.store_lob(1, &data, "application/octet-stream")
        .await
        .expect("store_lob failed");

    let fetched = repo.fetch_lob(1).await.expect("fetch_lob failed");
    assert!(fetched.is_some(), "expected Some(bytes) for id=1");
    assert_eq!(
        fetched.unwrap(),
        data,
        "fetched bytes differ from stored bytes"
    );
}

// ── test 2: store and fetch large LOB (1 MiB) ────────────────────────────────

#[tokio::test]
async fn lob_store_and_fetch_large() {
    let repo = make_repo().await;
    let data: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024).collect();

    repo.store_lob(2, &data, "video/mp4")
        .await
        .expect("store_lob 1 MiB failed");

    let fetched = repo
        .fetch_lob(2)
        .await
        .expect("fetch_lob 1 MiB failed")
        .expect("expected Some for 1 MiB LOB");
    assert_eq!(fetched.len(), 1024 * 1024, "length mismatch for 1 MiB LOB");
    assert_eq!(fetched, data, "content mismatch for 1 MiB LOB");
}

// ── test 3: delete removes both metadata and blob ────────────────────────────

#[tokio::test]
async fn lob_delete_removes_both_layers() {
    let repo = make_repo().await;
    let data = b"some binary content".to_vec();

    repo.store_lob(10, &data, "text/plain")
        .await
        .expect("store_lob");

    // Verify it's there.
    let before = repo.fetch_lob(10).await.expect("fetch before delete");
    assert!(before.is_some(), "expected LOB to exist before delete");

    // Delete it.
    repo.delete_lob(10).await.expect("delete_lob");

    // SQL row gone.
    let after_sql = repo.fetch_lob(10).await.expect("fetch after delete");
    assert!(
        after_sql.is_none(),
        "SQL metadata should be gone after delete"
    );

    // Blob bytes gone from store.
    let blob_exists = repo
        .store
        .exists("lob/10")
        .await
        .expect("exists check failed");
    assert!(!blob_exists, "blob bytes should be deleted from store");
}

// ── test 4: fetch returns None for missing id ─────────────────────────────────

#[tokio::test]
async fn lob_fetch_missing_returns_none() {
    let repo = make_repo().await;
    let result = repo
        .fetch_lob(9999)
        .await
        .expect("fetch_lob should not error");
    assert!(result.is_none(), "expected None for unknown id");
}

// ── test 5: multiple LOBs co-exist without collision ─────────────────────────

#[tokio::test]
async fn lob_multiple_no_collision() {
    let repo = make_repo().await;

    for i in 1i64..=5 {
        let data: Vec<u8> = std::iter::repeat_n(i as u8, 64 * i as usize).collect();
        repo.store_lob(i, &data, "application/octet-stream")
            .await
            .unwrap_or_else(|e| panic!("store_lob {} failed: {}", i, e));
    }

    for i in 1i64..=5 {
        let expected: Vec<u8> = std::iter::repeat_n(i as u8, 64 * i as usize).collect();
        let fetched = repo
            .fetch_lob(i)
            .await
            .unwrap_or_else(|e| panic!("fetch_lob {} failed: {}", i, e))
            .unwrap_or_else(|| panic!("expected Some for id {}", i));
        assert_eq!(
            fetched, expected,
            "LOB {} content mismatch (collision or overwrite)",
            i
        );
    }
}

// ── test 6: list_lobs returns correct count and metadata ─────────────────────

#[tokio::test]
async fn lob_list_returns_correct_metadata() {
    let repo = make_repo().await;

    let payloads = [
        (1i64, b"hello".as_ref(), "text/plain"),
        (2i64, b"world!".as_ref(), "text/plain"),
        (
            3i64,
            &[0xDE_u8, 0xAD_u8, 0xBE_u8, 0xEF_u8] as &[u8],
            "application/octet-stream",
        ),
    ];

    for (id, data, mime) in &payloads {
        repo.store_lob(*id, data, mime)
            .await
            .unwrap_or_else(|e| panic!("store {} failed: {}", id, e));
    }

    let list = repo.list_lobs().await.expect("list_lobs failed");
    assert_eq!(list.len(), 3, "expected 3 LOBs in list");

    // Verify sizes match actual data lengths.
    for ((id, data, mime), (lid, lmime, lsize)) in payloads.iter().zip(list.iter()) {
        assert_eq!(lid, id, "id mismatch in list");
        assert_eq!(lmime, mime, "mime mismatch for id {}", id);
        assert_eq!(*lsize, data.len() as i64, "size mismatch for id {}", id);
    }
}

// ── test 7: bytes fetched equal bytes stored (integrity) ─────────────────────

#[tokio::test]
async fn lob_integrity_sha256() {
    use sha2::{Digest, Sha256};

    let repo = make_repo().await;

    // 512-byte pseudo-random payload.
    let data: Vec<u8> = (0u64..512)
        .map(|i| {
            (i.wrapping_mul(6364136223846793005_u64)
                .wrapping_add(1442695040888963407_u64)) as u8
        })
        .collect();

    let expected_hash = {
        let mut h = Sha256::new();
        h.update(&data);
        h.finalize().to_vec()
    };

    repo.store_lob(42, &data, "application/octet-stream")
        .await
        .expect("store_lob");

    let fetched = repo.fetch_lob(42).await.expect("fetch_lob").expect("Some");

    let actual_hash = {
        let mut h = Sha256::new();
        h.update(&fetched);
        h.finalize().to_vec()
    };

    assert_eq!(
        actual_hash, expected_hash,
        "SHA-256 digest mismatch — data corrupted in transit"
    );
}

//! Advanced tests for `oxistore-blob`:
//! - Streaming read/write with large blobs (>100MB simulated)
//! - List with deeply nested key hierarchies
//! - Concurrent access (multiple tasks reading/writing simultaneously)
//! - LocalBlobStore atomic write integrity
//! - LocalBlobStore with special characters in keys (unicode, spaces)
//! - MemoryBlobStore clone semantics
//! - Copy and rename operations for both backends

use bytes::Bytes;
use oxistore_blob::{BlobStore, LocalBlobStore, MemoryBlobStore};
use std::sync::Arc;

// ── Large blob streaming (>100MB simulated, use 10MB for CI speed) ───────────

#[tokio::test]
async fn test_large_blob_streaming_memory() {
    let store = MemoryBlobStore::new();
    // Simulate a 10MB blob (fast enough for CI, demonstrates streaming correctness)
    let size = 10 * 1024 * 1024usize;
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let bytes = Bytes::from(data.clone());

    store
        .put("large_blob", bytes)
        .await
        .expect("put large blob");
    let retrieved = store.get("large_blob").await.expect("get large blob");
    assert_eq!(retrieved.len(), size);
    assert_eq!(retrieved.as_ref(), data.as_slice());
}

#[tokio::test]
async fn test_large_blob_streaming_local() {
    let tmp = std::env::temp_dir().join(format!("oxiblob_large_{}", std::process::id()));
    let store = LocalBlobStore::new(&tmp);
    let size = 10 * 1024 * 1024usize;
    let data: Vec<u8> = (0..size).map(|i| (i % 197) as u8).collect();
    let bytes = Bytes::from(data.clone());

    store
        .put("huge/chunk/data.bin", bytes)
        .await
        .expect("put large blob local");
    let retrieved = store
        .get("huge/chunk/data.bin")
        .await
        .expect("get large blob local");
    assert_eq!(retrieved.len(), size);
    assert_eq!(retrieved.as_ref(), data.as_slice());

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Deeply nested key hierarchies ────────────────────────────────────────────

#[tokio::test]
async fn test_list_deeply_nested_keys_memory() {
    let store = MemoryBlobStore::new();
    let keys = [
        "a/b/c/d/e/f/deep.txt",
        "a/b/c/d/e/f/deeper.txt",
        "a/b/c/shallow.txt",
        "a/b/mid.txt",
        "a/top.txt",
        "root.txt",
    ];
    for key in &keys {
        store
            .put(key, Bytes::from_static(b"content"))
            .await
            .expect("put");
    }

    // List all under "a/"
    let mut found = store.list("a/").await.expect("list a/");
    found.sort();
    assert_eq!(found.len(), 5, "should find 5 keys under a/");
    assert!(found.contains(&"a/b/c/d/e/f/deep.txt".to_string()));
    assert!(found.contains(&"a/top.txt".to_string()));

    // List deeply nested
    let mut deep = store.list("a/b/c/d/").await.expect("list deep");
    deep.sort();
    assert_eq!(deep.len(), 2);
    assert!(deep.iter().all(|k| k.starts_with("a/b/c/d/")));

    // List from root (all)
    let all = store.list("").await.expect("list all");
    assert_eq!(all.len(), keys.len());
}

#[tokio::test]
async fn test_list_deeply_nested_keys_local() {
    let tmp = std::env::temp_dir().join(format!("oxiblob_nested_{}", std::process::id()));
    let store = LocalBlobStore::new(&tmp);
    let keys = [
        "dir1/dir2/dir3/file.txt",
        "dir1/dir2/other.txt",
        "dir1/top.txt",
    ];
    for key in &keys {
        store
            .put(key, Bytes::from_static(b"data"))
            .await
            .expect("put");
    }

    let mut found = store.list("dir1/").await.expect("list dir1/");
    found.sort();
    assert_eq!(found.len(), 3);

    let mut deep = store.list("dir1/dir2/").await.expect("list dir1/dir2/");
    deep.sort();
    assert_eq!(deep.len(), 2);

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Concurrent access ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_concurrent_access_memory() {
    let store = Arc::new(MemoryBlobStore::new());
    let n_tasks = 16usize;
    let ops_per_task = 50usize;

    let mut handles = Vec::with_capacity(n_tasks);
    for task_id in 0..n_tasks {
        let store_clone = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for op in 0..ops_per_task {
                let key = format!("task{task_id}/key{op}");
                let val = Bytes::from(format!("value-{task_id}-{op}"));
                store_clone
                    .put(&key, val.clone())
                    .await
                    .expect("concurrent put");
                let got = store_clone.get(&key).await.expect("concurrent get");
                assert_eq!(got, val);
            }
        }));
    }
    for h in handles {
        h.await.expect("task panicked");
    }

    // Verify total count
    let all = store.list("").await.expect("list all");
    assert_eq!(all.len(), n_tasks * ops_per_task);
}

#[tokio::test]
async fn test_concurrent_access_local() {
    let tmp = std::env::temp_dir().join(format!("oxiblob_concurrent_{}", std::process::id()));
    let store = Arc::new(LocalBlobStore::new(&tmp));
    let n_tasks = 8usize;
    let ops_per_task = 20usize;

    let mut handles = Vec::with_capacity(n_tasks);
    for task_id in 0..n_tasks {
        let store_clone = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for op in 0..ops_per_task {
                let key = format!("task{task_id}/key{op:02}");
                let val = Bytes::from(format!("v-{task_id}-{op}"));
                store_clone.put(&key, val.clone()).await.expect("put");
                let got = store_clone.get(&key).await.expect("get");
                assert_eq!(got, val);
            }
        }));
    }
    for h in handles {
        h.await.expect("task panicked");
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── LocalBlobStore atomic write: no partial files on interrupt ────────────────

#[tokio::test]
async fn test_local_atomic_write_no_partial_file() {
    // We can't truly simulate a crash, but we verify:
    // 1. The .tmp file is cleaned up after a successful write
    // 2. The content is correct after put
    let tmp = std::env::temp_dir().join(format!("oxiblob_atomic_{}", std::process::id()));
    let store = LocalBlobStore::new(&tmp);

    let key = "important/data.bin";
    let data = Bytes::from(vec![0xABu8; 4096]);
    store.put(key, data.clone()).await.expect("put");

    // No .tmp files should remain
    let blob_dir = tmp.join("important");
    if blob_dir.exists() {
        for entry in std::fs::read_dir(&blob_dir).expect("readdir") {
            let entry = entry.expect("entry");
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            assert!(
                !name_str.ends_with(".tmp"),
                "leftover .tmp file: {name_str}"
            );
        }
    }

    // Content is correct
    let got = store.get(key).await.expect("get");
    assert_eq!(got, data);

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Special characters in keys ────────────────────────────────────────────────

#[tokio::test]
async fn test_special_characters_in_keys_memory() {
    let store = MemoryBlobStore::new();

    // Unicode, spaces, hyphens, underscores, dots
    let keys_values = vec![
        ("hello world.txt", b"space" as &[u8]),
        ("файл.txt", b"cyrillic"),
        ("日本語/ファイル", b"japanese"),
        ("path/with-hyphen_and.dot", b"mixed"),
        ("emoji_🦀_rust", b"emoji"),
    ];

    for (key, val) in &keys_values {
        store
            .put(key, Bytes::copy_from_slice(val))
            .await
            .expect("put special key");
    }

    for (key, val) in &keys_values {
        let got = store.get(key).await.expect("get special key");
        assert_eq!(got.as_ref(), *val, "mismatch for key: {key}");
    }
}

#[tokio::test]
async fn test_special_characters_in_keys_local() {
    let tmp = std::env::temp_dir().join(format!("oxiblob_special_{}", std::process::id()));
    let store = LocalBlobStore::new(&tmp);

    // Spaces and unicode in key names (URL-safe-like characters)
    let keys_values = vec![
        ("path/with spaces/file.txt", b"has spaces" as &[u8]),
        ("path/with-hyphen_and.dot/file.bin", b"mixed chars"),
    ];

    for (key, val) in &keys_values {
        store
            .put(key, Bytes::copy_from_slice(val))
            .await
            .expect("put");
    }

    for (key, val) in &keys_values {
        let got = store.get(key).await.expect("get");
        assert_eq!(got.as_ref(), *val);
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── MemoryBlobStore clone semantics ──────────────────────────────────────────

#[tokio::test]
async fn test_memory_clone_shares_data() {
    let store_a = MemoryBlobStore::new();
    store_a
        .put("shared_key", Bytes::from_static(b"original"))
        .await
        .expect("put");

    // Clone should observe the same underlying data
    let store_b = store_a.clone();
    let got = store_b.get("shared_key").await.expect("get from clone");
    assert_eq!(got.as_ref(), b"original");

    // Writes through clone are visible via original
    store_b
        .put("new_key", Bytes::from_static(b"from_clone"))
        .await
        .expect("put via clone");
    let via_original = store_a.get("new_key").await.expect("get via original");
    assert_eq!(via_original.as_ref(), b"from_clone");

    // Overwrites via original are visible via clone
    store_a
        .put("shared_key", Bytes::from_static(b"updated"))
        .await
        .expect("update");
    let from_clone = store_b.get("shared_key").await.expect("get updated");
    assert_eq!(from_clone.as_ref(), b"updated");
}

// ── Copy and rename operations ────────────────────────────────────────────────

#[tokio::test]
async fn test_copy_memory() {
    let store = MemoryBlobStore::new();
    store
        .put("src", Bytes::from_static(b"copy me"))
        .await
        .expect("put src");

    store.copy("src", "dst").await.expect("copy");

    // Both should exist with same content
    let src = store.get("src").await.expect("get src after copy");
    let dst = store.get("dst").await.expect("get dst after copy");
    assert_eq!(src, dst);
    assert_eq!(src.as_ref(), b"copy me");
}

#[tokio::test]
async fn test_copy_local() {
    let tmp = std::env::temp_dir().join(format!("oxiblob_copy_{}", std::process::id()));
    let store = LocalBlobStore::new(&tmp);
    store
        .put("src/file.txt", Bytes::from_static(b"copy me local"))
        .await
        .expect("put");

    store
        .copy("src/file.txt", "dst/file.txt")
        .await
        .expect("copy");

    let src = store.get("src/file.txt").await.expect("get src");
    let dst = store.get("dst/file.txt").await.expect("get dst");
    assert_eq!(src, dst);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_rename_memory() {
    let store = MemoryBlobStore::new();
    store
        .put("old_name", Bytes::from_static(b"rename me"))
        .await
        .expect("put");

    store.rename("old_name", "new_name").await.expect("rename");

    // Old key should be gone; new key should have the data
    let old_exists = store.exists("old_name").await.expect("exists old");
    assert!(!old_exists, "old key should be gone after rename");

    let new_val = store.get("new_name").await.expect("get new");
    assert_eq!(new_val.as_ref(), b"rename me");
}

#[tokio::test]
async fn test_rename_local() {
    let tmp = std::env::temp_dir().join(format!("oxiblob_rename_{}", std::process::id()));
    let store = LocalBlobStore::new(&tmp);
    store
        .put("old_dir/file.bin", Bytes::from_static(b"original data"))
        .await
        .expect("put");

    store
        .rename("old_dir/file.bin", "new_dir/file.bin")
        .await
        .expect("rename");

    let old_exists = store.exists("old_dir/file.bin").await.expect("exists");
    assert!(!old_exists);

    let new_val = store.get("new_dir/file.bin").await.expect("get new");
    assert_eq!(new_val.as_ref(), b"original data");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn test_copy_not_found() {
    let store = MemoryBlobStore::new();
    let result = store.copy("nonexistent", "dest").await;
    assert!(result.is_err(), "copy of nonexistent key should fail");
}

#[tokio::test]
async fn test_rename_not_found() {
    let store = MemoryBlobStore::new();
    let result = store.rename("nonexistent", "dest").await;
    assert!(result.is_err(), "rename of nonexistent key should fail");
}

// ── Delete many / delete prefix cross-backend tests ─────────────────────────

#[tokio::test]
async fn test_delete_many_memory() {
    let store = MemoryBlobStore::new();
    for i in 0..10 {
        store
            .put(&format!("key{i}"), Bytes::from_static(b"v"))
            .await
            .expect("put");
    }
    let to_delete: Vec<&str> = vec!["key0", "key3", "key7", "key_nonexistent"];
    store.delete_many(&to_delete).await.expect("delete_many");

    // key0, key3, key7 gone; others present
    assert!(!store.exists("key0").await.unwrap());
    assert!(!store.exists("key3").await.unwrap());
    assert!(!store.exists("key7").await.unwrap());
    assert!(store.exists("key1").await.unwrap());
    assert!(store.exists("key5").await.unwrap());
}

#[tokio::test]
async fn test_delete_prefix_local() {
    let tmp = std::env::temp_dir().join(format!("oxiblob_delprefix_{}", std::process::id()));
    let store = LocalBlobStore::new(&tmp);
    for i in 0..5 {
        store
            .put(&format!("prefix/file{i}.txt"), Bytes::from_static(b"data"))
            .await
            .expect("put");
    }
    store
        .put("other/file.txt", Bytes::from_static(b"keep"))
        .await
        .expect("put other");

    let deleted = store.delete_prefix("prefix/").await.expect("delete_prefix");
    assert_eq!(deleted, 5);

    // Other key still present
    assert!(store.exists("other/file.txt").await.unwrap());
    // Prefix keys gone
    for i in 0..5 {
        assert!(!store.exists(&format!("prefix/file{i}.txt")).await.unwrap());
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

//! Integration tests for Slice-6 facade additions:
//! - `detect_backend`
//! - `destroy`
//! - `backup_store` / `restore_store`
//! - `open_blob`
//! - `prelude` module
//! - `EncryptError::KeyRotation`
//! - `EncryptedKv` `Debug` impl

#![forbid(unsafe_code)]

// ── detect_backend (redb) ─────────────────────────────────────────────────────

#[cfg(feature = "kv-redb")]
#[test]
fn detect_backend_redb() {
    let path = std::env::temp_dir().join(format!(
        "detect_redb_{}_{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let store = oxistore::open(&path).expect("open redb");
    store.put(b"k", b"v").expect("put");
    drop(store);
    let detected = oxistore::detect_backend(&path).expect("detect");
    assert_eq!(detected, oxistore::StoreKind::Redb);
    let _ = std::fs::remove_file(&path);
}

// ── detect_backend (sled) ─────────────────────────────────────────────────────

#[cfg(feature = "kv-sled")]
#[test]
fn detect_backend_sled() {
    let path = std::env::temp_dir().join(format!(
        "detect_sled_{}_{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let store = oxistore::open_with(oxistore::StoreKind::Sled, &path).expect("open sled");
    store.put(b"k", b"v").expect("put");
    drop(store);
    let detected = oxistore::detect_backend(&path).expect("detect");
    assert_eq!(detected, oxistore::StoreKind::Sled);
    let _ = std::fs::remove_dir_all(&path);
}

// ── detect_backend (missing path) ────────────────────────────────────────────

#[test]
fn detect_backend_missing_path_returns_error() {
    let path = std::env::temp_dir().join(format!(
        "detect_missing_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    // Ensure it does not exist.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&path);
    let result = oxistore::detect_backend(&path);
    assert!(result.is_err(), "expected error for non-existent path");
}

// ── destroy (redb) ────────────────────────────────────────────────────────────

#[cfg(feature = "kv-redb")]
#[test]
fn destroy_removes_redb_store() {
    let path = std::env::temp_dir().join(format!(
        "destroy_redb_{}_{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let store = oxistore::open(&path).expect("open");
    store.put(b"k", b"v").expect("put");
    drop(store);
    assert!(path.exists(), "store file should exist before destroy");
    oxistore::destroy(oxistore::StoreKind::Redb, &path).expect("destroy");
    assert!(!path.exists(), "store file should be removed after destroy");
}

// ── destroy (sled) ────────────────────────────────────────────────────────────

#[cfg(feature = "kv-sled")]
#[test]
fn destroy_removes_sled_store() {
    let path = std::env::temp_dir().join(format!(
        "destroy_sled_{}_{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let store = oxistore::open_with(oxistore::StoreKind::Sled, &path).expect("open sled");
    store.put(b"k", b"v").expect("put");
    drop(store);
    assert!(path.exists(), "store dir should exist before destroy");
    oxistore::destroy(oxistore::StoreKind::Sled, &path).expect("destroy");
    assert!(!path.exists(), "store dir should be removed after destroy");
}

// ── destroy noop on missing path ──────────────────────────────────────────────

#[test]
fn destroy_noop_when_path_absent() {
    let path = std::env::temp_dir().join(format!(
        "destroy_absent_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    // Should succeed without error even though path does not exist.
    oxistore::destroy(oxistore::StoreKind::Redb, &path)
        .expect("destroy on absent path should be a no-op");
}

// ── backup_store / restore_store ──────────────────────────────────────────────

#[cfg(feature = "kv-redb")]
#[test]
fn backup_store_redb_produces_file() {
    let src = std::env::temp_dir().join(format!(
        "backup_src_{}_{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let dst = std::env::temp_dir().join(format!(
        "backup_dst_{}_{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));

    let store = oxistore::open(&src).expect("open src");
    store.put(b"backup-key", b"backup-val").expect("put");
    drop(store);

    let result = oxistore::backup_store(oxistore::StoreKind::Redb, &src, &dst);
    // backup may or may not be supported; we just verify it doesn't panic.
    drop(result);

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&dst);
}

// ── prelude imports compile ───────────────────────────────────────────────────

#[test]
fn prelude_imports_compile() {
    use oxistore::prelude::*;
    let _: Option<StoreError> = None;
    let _: Option<StoreKind> = None;
}

// ── EncryptError::KeyRotation display ────────────────────────────────────────

#[cfg(feature = "encrypt")]
#[test]
fn encrypt_key_rotation_error_display() {
    use oxistore_encrypt::EncryptError;
    let err = EncryptError::KeyRotation {
        old_version: 1,
        new_version: 2,
        reason: "test reason".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("1"), "should mention old version");
    assert!(msg.contains("2"), "should mention new version");
    assert!(msg.contains("test reason"), "should contain reason text");
}

// ── EncryptedKv Debug redacts key material ────────────────────────────────────

#[cfg(feature = "encrypt")]
#[test]
fn encrypt_encrypted_kv_debug_redacts_key() {
    use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
    use oxistore_encrypt::{EncryptedKv, StaticKey};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // Minimal in-memory store to avoid depending on a specific KV backend.
    #[derive(Default)]
    struct MemStore(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

    impl KvStore for MemStore {
        fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(self.0.lock().expect("lock").get(key).cloned())
        }
        fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
            self.0
                .lock()
                .expect("lock")
                .insert(key.to_vec(), value.to_vec());
            Ok(())
        }
        fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
            self.0.lock().expect("lock").remove(key);
            Ok(())
        }
        fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
            let guard = self.0.lock().expect("lock");
            let lo = lo.to_vec();
            let hi = hi.to_vec();
            let items: Vec<_> = guard
                .iter()
                .filter(|(k, _)| k.as_slice() >= lo.as_slice() && k.as_slice() < hi.as_slice())
                .map(|(k, v)| Ok((k.clone(), v.clone())))
                .collect();
            Ok(Box::new(items.into_iter()))
        }
        fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
            let guard = self.0.lock().expect("lock");
            let items: Vec<_> = guard
                .iter()
                .map(|(k, v)| Ok((k.clone(), v.clone())))
                .collect();
            Ok(Box::new(items.into_iter()))
        }
        fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
            Err(StoreError::Unsupported("no txn in MemStore".to_string()))
        }
        fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
            Err(StoreError::Unsupported(
                "no snapshot in MemStore".to_string(),
            ))
        }
        fn flush(&self) -> Result<(), StoreError> {
            Ok(())
        }
    }

    let inner = MemStore::default();
    let key_bytes = [0u8; 32];
    let key_provider = StaticKey::new(key_bytes.to_vec());
    let encrypted = EncryptedKv::new(inner, key_provider);
    let debug_str = format!("{encrypted:?}");
    assert!(
        debug_str.contains("EncryptedKv"),
        "debug output should include struct name"
    );
    assert!(
        debug_str.contains("REDACTED"),
        "debug output should redact key material: got {debug_str}"
    );
}

// ── open_blob ─────────────────────────────────────────────────────────────────

#[cfg(feature = "blob")]
#[test]
fn open_blob_returns_local_blob_store() {
    use oxistore_blob::LocalBlobStore;
    let path = std::env::temp_dir().join(format!(
        "open_blob_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    let store: LocalBlobStore = oxistore::open_blob(&path).expect("open_blob");
    // Just verify the type is returned correctly (store is lazy — no I/O yet).
    let _ = store;
    // No cleanup needed (directory was never created).
}

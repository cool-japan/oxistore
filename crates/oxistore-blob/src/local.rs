//! Filesystem-backed [`crate::BlobStore`] implementation.
//!
//! Keys are mapped directly to paths under a single base directory.  Writes
//! are made durable via an atomic rename: the payload is first written to a
//! sibling `.tmp` file and then renamed into place, so readers never observe a
//! partially-written blob.

use std::future::Future;
use std::path::PathBuf;

use bytes::Bytes;

use crate::error::BlobError;
use crate::{BlobMeta, BlobStore};

/// A blob store backed by the local filesystem.
///
/// All blobs are stored as flat files under the `base` directory.  Keys with
/// `/` separators create nested subdirectories.
///
/// # Key restrictions
///
/// A key must be non-empty and must not contain the `..` path component,
/// preventing directory-traversal attacks.  Any such key returns
/// [`BlobError::Other`].
///
/// # Checksum verification
///
/// When constructed via [`LocalBlobStore::with_checksum_verification`], every
/// [`BlobStore::get`] call re-computes the SHA-256 of the returned bytes and
/// compares it with the stored CAS digest.  If the stored key is **not** a
/// hex-encoded SHA-256 (e.g. an ordinary named key) the verification is skipped
/// and the bytes are returned as-is.  If a CAS digest is found and the
/// re-computed hash diverges, [`BlobError::ChecksumMismatch`] is returned.
#[derive(Debug, Clone)]
pub struct LocalBlobStore {
    base: PathBuf,
    /// When `true`, [`BlobStore::get`] re-verifies the SHA-256 of the returned
    /// bytes against the key (which must be a hex-encoded SHA-256 digest).
    verify_checksum: bool,
}

impl LocalBlobStore {
    /// Create a new store rooted at `base`.
    ///
    /// The directory is created lazily on the first `put` call.
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            verify_checksum: false,
        }
    }

    /// Create a new store rooted at `base` with SHA-256 checksum verification
    /// enabled on every [`BlobStore::get`].
    ///
    /// When verification is enabled, each `get` call compares the SHA-256 of
    /// the returned bytes against the key (if it looks like a hex digest).
    /// Ordinary named keys are returned as-is.
    pub fn with_checksum_verification(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            verify_checksum: true,
        }
    }

    /// Create a new store rooted at `base` and immediately remove any leftover
    /// `*.tmp` files from a previous interrupted write session.
    ///
    /// This is useful when you want a clean state on application startup.
    /// The scan is synchronous and recursive — for very large stores, prefer
    /// running this in a blocking task.
    ///
    /// # Errors
    ///
    /// Returns `Err` only if the base directory exists but cannot be read.
    /// Missing base directory is silently ignored (nothing to clean up).
    pub fn with_temp_cleanup(base: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let base_path = base.into();
        if base_path.exists() {
            cleanup_tmp_files_sync(&base_path)?;
        }
        Ok(Self {
            base: base_path,
            verify_checksum: false,
        })
    }

    /// Clean up leftover `*.tmp` files under `base` synchronously.
    ///
    /// Same as [`LocalBlobStore::with_temp_cleanup`] but on an existing instance.
    /// Returns the number of temp files removed.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the directory cannot be read.
    pub fn cleanup_temp_files(&self) -> Result<u64, std::io::Error> {
        cleanup_tmp_files_sync(&self.base)
    }

    /// Resolve a key to an absolute path, rejecting unsafe keys.
    fn resolve(&self, key: &str) -> Result<PathBuf, BlobError> {
        validate_key(key)?;
        Ok(self.base.join(key))
    }
}

/// Reject empty keys or keys containing `..`.
fn validate_key(key: &str) -> Result<(), BlobError> {
    if key.is_empty() {
        return Err(BlobError::Other("key must not be empty".to_string()));
    }
    // Reject any path component that is exactly `..`.
    for component in std::path::Path::new(key).components() {
        if component == std::path::Component::ParentDir {
            return Err(BlobError::Other(
                "key must not contain '..' path component".to_string(),
            ));
        }
    }
    Ok(())
}

impl BlobStore for LocalBlobStore {
    fn put(&self, key: &str, data: Bytes) -> impl Future<Output = Result<(), BlobError>> + Send {
        let path = self.resolve(key);
        async move {
            let path = path?;
            // Ensure parent directory exists.
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            // Atomic write: write to a temp file then rename.
            let tmp_path = path.with_extension("tmp");
            tokio::fs::write(&tmp_path, &data).await?;
            tokio::fs::rename(&tmp_path, &path).await?;
            Ok(())
        }
    }

    fn get(&self, key: &str) -> impl Future<Output = Result<Bytes, BlobError>> + Send {
        let path = self.resolve(key);
        let key_owned = key.to_string();
        let verify = self.verify_checksum;
        async move {
            let path = path?;
            let bytes = match tokio::fs::read(&path).await {
                Ok(b) => Bytes::from(b),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(BlobError::NotFound(key_from_path(&path)));
                }
                Err(e) => return Err(BlobError::Io(e)),
            };
            if verify {
                // Only verify if the key looks like a hex-encoded SHA-256 (64 hex chars).
                if key_owned.len() == 64 && key_owned.chars().all(|c| c.is_ascii_hexdigit()) {
                    let actual = crate::cas::sha256(&bytes);
                    let actual_hex = actual.to_hex();
                    if actual_hex != key_owned {
                        return Err(BlobError::ChecksumMismatch(format!(
                            "key {key_owned}: stored content has SHA-256 {actual_hex}"
                        )));
                    }
                }
            }
            Ok(bytes)
        }
    }

    fn delete(&self, key: &str) -> impl Future<Output = Result<(), BlobError>> + Send {
        let path = self.resolve(key);
        let key_owned = key.to_string();
        async move {
            let path = path?;
            match tokio::fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Err(BlobError::NotFound(key_owned))
                }
                Err(e) => Err(BlobError::Io(e)),
            }
        }
    }

    fn head(&self, key: &str) -> impl Future<Output = Result<BlobMeta, BlobError>> + Send {
        let path = self.resolve(key);
        let key_owned = key.to_string();
        async move {
            let path = path?;
            match tokio::fs::metadata(&path).await {
                Ok(meta) => Ok(BlobMeta {
                    key: key_owned,
                    size: meta.len(),
                    content_type: None,
                    checksum: None,
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Err(BlobError::NotFound(key_owned))
                }
                Err(e) => Err(BlobError::Io(e)),
            }
        }
    }

    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, BlobError>> + Send {
        let base = self.base.clone();
        let prefix_owned = prefix.to_string();
        async move {
            let mut result = Vec::new();
            collect_entries(&base, &base, &prefix_owned, &mut result).await?;
            result.sort();
            Ok(result)
        }
    }

    fn list_meta(
        &self,
        prefix: &str,
    ) -> impl Future<Output = Result<Vec<crate::BlobMeta>, BlobError>> + Send {
        let base = self.base.clone();
        let prefix_owned = prefix.to_string();
        async move {
            let mut keys = Vec::new();
            collect_entries(&base, &base, &prefix_owned, &mut keys).await?;
            keys.sort();
            let mut metas = Vec::with_capacity(keys.len());
            for key in keys {
                let path = base.join(&key);
                match tokio::fs::metadata(&path).await {
                    Ok(meta) => metas.push(crate::BlobMeta {
                        key,
                        size: meta.len(),
                        content_type: None,
                        checksum: None,
                    }),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        // Blob removed between list and stat — skip it.
                    }
                    Err(e) => return Err(BlobError::Io(e)),
                }
            }
            Ok(metas)
        }
    }

    fn list_meta_page(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<crate::BlobMeta>, BlobError>> + Send {
        let base = self.base.clone();
        let prefix_owned = prefix.to_string();
        let start_after_owned = start_after.map(str::to_string);
        async move {
            let mut keys = Vec::new();
            collect_entries(&base, &base, &prefix_owned, &mut keys).await?;
            keys.sort();
            let mut metas = Vec::with_capacity(limit.min(keys.len()));
            for key in keys {
                if let Some(ref sa) = start_after_owned {
                    if key.as_str() <= sa.as_str() {
                        continue;
                    }
                }
                if metas.len() >= limit {
                    break;
                }
                let path = base.join(&key);
                match tokio::fs::metadata(&path).await {
                    Ok(meta) => metas.push(crate::BlobMeta {
                        key,
                        size: meta.len(),
                        content_type: None,
                        checksum: None,
                    }),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        // Blob removed between list and stat — skip it.
                    }
                    Err(e) => return Err(BlobError::Io(e)),
                }
            }
            Ok(metas)
        }
    }
}

/// Recursively remove all `*.tmp` files under `dir`.
///
/// Returns the total count of removed files.  Errors accessing individual
/// entries are silently ignored (best-effort cleanup).
fn cleanup_tmp_files_sync(dir: &std::path::Path) -> Result<u64, std::io::Error> {
    let mut count = 0u64;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            count += cleanup_tmp_files_sync(&path).unwrap_or(0);
        } else if file_type.is_file()
            && path.extension().is_some_and(|ext| ext == "tmp")
            && std::fs::remove_file(&path).is_ok()
        {
            count += 1;
        }
    }
    Ok(count)
}

/// Recursively walk `dir`, collecting relative key strings that start with `prefix`.
///
/// This is a recursive async helper.  It uses a `Box::pin` future to satisfy
/// the compiler's requirement that recursive `async fn`s be pinned.
fn collect_entries<'a>(
    base: &'a PathBuf,
    dir: &'a PathBuf,
    prefix: &'a str,
    result: &'a mut Vec<String>,
) -> std::pin::Pin<Box<dyn Future<Output = Result<(), BlobError>> + Send + 'a>> {
    Box::pin(async move {
        let mut read_dir = match tokio::fs::read_dir(dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(BlobError::Io(e)),
        };

        while let Some(entry) = read_dir.next_entry().await? {
            let entry_path = entry.path();
            let file_type = entry.file_type().await?;

            if file_type.is_dir() {
                collect_entries(base, &entry_path, prefix, result).await?;
            } else if file_type.is_file() {
                // Skip temp files left by interrupted writes.
                if let Some(ext) = entry_path.extension() {
                    if ext == "tmp" {
                        continue;
                    }
                }
                // Convert the absolute path back to a relative key string.
                if let Ok(rel) = entry_path.strip_prefix(base) {
                    if let Some(key) = rel.to_str() {
                        // Normalize path separators to '/' on all platforms.
                        let key_str = key.replace(std::path::MAIN_SEPARATOR, "/");
                        if key_str.starts_with(prefix) {
                            result.push(key_str);
                        }
                    }
                }
            }
        }
        Ok(())
    })
}

/// Extract a display key string from an absolute path (best-effort).
fn key_from_path(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unknown>")
        .to_string()
}

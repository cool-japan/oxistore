//! In-memory [`crate::BlobStore`] implementation backed by a `BTreeMap`.
//!
//! All data is stored in heap memory for the lifetime of the store.  This
//! backend is primarily intended for unit tests and local development; it does
//! not persist data across restarts.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::RwLock;

use crate::error::BlobError;
use crate::{BlobMeta, BlobStore};

/// Shared state for [`MemoryBlobStore`].
#[derive(Debug, Default)]
struct MemoryState {
    map: BTreeMap<String, Bytes>,
    /// Total bytes currently stored across all blobs.
    used_bytes: u64,
    /// Optional upper bound on total stored bytes.  `None` means unbounded.
    capacity_bytes: Option<u64>,
}

/// An in-memory blob store backed by a `BTreeMap<String, Bytes>`.
///
/// The inner map is wrapped in an `Arc<RwLock<_>>` so the store can be cloned
/// and shared across tasks; all clones observe the same underlying data.
///
/// If a `capacity_bytes` limit is configured via `BlobStoreBuilder`, any
/// `put` call that would exceed the limit returns
/// [`BlobError::QuotaExceeded`].
#[derive(Debug, Clone)]
pub struct MemoryBlobStore {
    inner: Arc<RwLock<MemoryState>>,
}

impl MemoryBlobStore {
    /// Create a new, empty in-memory store with no capacity limit.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(MemoryState::default())),
        }
    }

    /// Create an in-memory store with an explicit capacity limit in bytes.
    pub(crate) fn with_capacity(capacity_bytes: u64) -> Self {
        Self {
            inner: Arc::new(RwLock::new(MemoryState {
                map: BTreeMap::new(),
                used_bytes: 0,
                capacity_bytes: Some(capacity_bytes),
            })),
        }
    }
}

impl Default for MemoryBlobStore {
    fn default() -> Self {
        Self::new()
    }
}

impl BlobStore for MemoryBlobStore {
    fn put(&self, key: &str, data: Bytes) -> impl Future<Output = Result<(), BlobError>> + Send {
        let inner = Arc::clone(&self.inner);
        let key_owned = key.to_string();
        async move {
            let mut state = inner.write().await;

            // Determine bytes displaced by an existing blob at this key.
            let displaced = state.map.get(&key_owned).map_or(0u64, |b| b.len() as u64);
            let new_len = data.len() as u64;

            // Enforce capacity quota.
            if let Some(cap) = state.capacity_bytes {
                let would_be = state.used_bytes - displaced + new_len;
                if would_be > cap {
                    return Err(BlobError::QuotaExceeded {
                        limit_bytes: cap,
                        needed_bytes: would_be,
                    });
                }
            }

            state.used_bytes = state.used_bytes - displaced + new_len;
            state.map.insert(key_owned, data);
            Ok(())
        }
    }

    fn get(&self, key: &str) -> impl Future<Output = Result<Bytes, BlobError>> + Send {
        let inner = Arc::clone(&self.inner);
        let key_owned = key.to_string();
        async move {
            let state = inner.read().await;
            state
                .map
                .get(&key_owned)
                .cloned()
                .ok_or(BlobError::NotFound(key_owned))
        }
    }

    fn delete(&self, key: &str) -> impl Future<Output = Result<(), BlobError>> + Send {
        let inner = Arc::clone(&self.inner);
        let key_owned = key.to_string();
        async move {
            let mut state = inner.write().await;
            if let Some(removed) = state.map.remove(&key_owned) {
                state.used_bytes -= removed.len() as u64;
                Ok(())
            } else {
                Err(BlobError::NotFound(key_owned))
            }
        }
    }

    fn head(&self, key: &str) -> impl Future<Output = Result<BlobMeta, BlobError>> + Send {
        let inner = Arc::clone(&self.inner);
        let key_owned = key.to_string();
        async move {
            let state = inner.read().await;
            match state.map.get(&key_owned) {
                Some(data) => Ok(BlobMeta {
                    key: key_owned,
                    size: data.len() as u64,
                    content_type: None,
                    checksum: None,
                }),
                None => Err(BlobError::NotFound(key_owned)),
            }
        }
    }

    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, BlobError>> + Send {
        let inner = Arc::clone(&self.inner);
        let prefix_owned = prefix.to_string();
        async move {
            let state = inner.read().await;
            // BTreeMap iteration is already in sorted (lexicographic) order.
            let keys = state
                .map
                .keys()
                .filter(|k| k.starts_with(&prefix_owned))
                .cloned()
                .collect();
            Ok(keys)
        }
    }

    /// List metadata for all blobs whose key starts with `prefix`.
    ///
    /// Overrides the default trait implementation for efficiency — reads directly
    /// from the BTreeMap without the overhead of separate `head` calls.
    fn list_meta(
        &self,
        prefix: &str,
    ) -> impl Future<Output = Result<Vec<BlobMeta>, BlobError>> + Send {
        let inner = Arc::clone(&self.inner);
        let prefix_owned = prefix.to_string();
        async move {
            let state = inner.read().await;
            let metas = state
                .map
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix_owned))
                .map(|(k, v)| BlobMeta {
                    key: k.clone(),
                    size: v.len() as u64,
                    content_type: None,
                    checksum: None,
                })
                .collect();
            Ok(metas)
        }
    }

    /// List a page of metadata for blobs whose key starts with `prefix`.
    ///
    /// `start_after` is an exclusive lower bound (continuation token).
    /// `limit` caps the number of returned entries.
    ///
    /// Overrides the default trait implementation for efficiency.
    fn list_meta_page(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<BlobMeta>, BlobError>> + Send {
        let inner = Arc::clone(&self.inner);
        let prefix_owned = prefix.to_string();
        let start_after_owned = start_after.map(str::to_string);
        async move {
            let state = inner.read().await;
            let metas = state
                .map
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix_owned))
                .filter(|(k, _)| {
                    start_after_owned
                        .as_deref()
                        .is_none_or(|sa| k.as_str() > sa)
                })
                .take(limit)
                .map(|(k, v)| BlobMeta {
                    key: k.clone(),
                    size: v.len() as u64,
                    content_type: None,
                    checksum: None,
                })
                .collect();
            Ok(metas)
        }
    }
}

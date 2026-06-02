//! Typed key-value store adapter with configurable codec.
//!
//! This module provides [`TypedKvStore`], a wrapper over any [`KvStore`] that
//! transparently serialises/deserialises typed keys and values using a
//! pluggable [`TypedCodec`].  The built-in [`JsonCodec`] uses `serde_json`
//! (Pure Rust, always available).

use crate::{KvStore, StoreError};
use serde::{de::DeserializeOwned, Serialize};
use std::fmt::Display;

/// Codec for encoding/decoding typed values to/from bytes.
///
/// Implementations are stateless (or carry only configuration); they must be
/// `Send + Sync` so that [`TypedKvStore`] remains `Send + Sync`.
pub trait TypedCodec: Send + Sync {
    /// The error type produced when encoding or decoding fails.
    type Error: Display + std::fmt::Debug + Send + Sync + 'static;

    /// Encode `value` as a byte vector.
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` if serialisation fails.
    fn encode<V: Serialize>(&self, value: &V) -> Result<Vec<u8>, Self::Error>;

    /// Decode a `V` from `bytes`.
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` if deserialisation fails.
    fn decode<V: DeserializeOwned>(&self, bytes: &[u8]) -> Result<V, Self::Error>;

    /// Encode a key.  Default delegates to [`encode`](Self::encode).
    ///
    /// Override if keys require a different representation (e.g. lexicographic
    /// byte ordering rather than JSON's string representation).
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` if key serialisation fails.
    fn encode_key<K: Serialize>(&self, key: &K) -> Result<Vec<u8>, Self::Error> {
        self.encode(key)
    }
}

// ── JsonCodec ─────────────────────────────────────────────────────────────────

/// JSON codec using `serde_json` — Pure Rust, always available.
///
/// Keys are encoded as their JSON representation (e.g. `"\"my_key\""` for a
/// `String`).  Values are likewise encoded as compact JSON.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonCodec;

impl TypedCodec for JsonCodec {
    type Error = serde_json::Error;

    fn encode<V: Serialize>(&self, value: &V) -> Result<Vec<u8>, Self::Error> {
        serde_json::to_vec(value)
    }

    fn decode<V: DeserializeOwned>(&self, bytes: &[u8]) -> Result<V, Self::Error> {
        serde_json::from_slice(bytes)
    }
}

// ── TypedKvError ──────────────────────────────────────────────────────────────

/// Error type for [`TypedKvStore`] operations.
#[derive(Debug)]
pub enum TypedKvError<E: Display + std::fmt::Debug + Send + Sync + 'static> {
    /// A codec (serialisation/deserialisation) error.
    Codec(E),
    /// An error returned by the underlying [`KvStore`].
    Store(StoreError),
}

impl<E: Display + std::fmt::Debug + Send + Sync + 'static> Display for TypedKvError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypedKvError::Codec(e) => write!(f, "codec error: {e}"),
            TypedKvError::Store(e) => write!(f, "store error: {e}"),
        }
    }
}

impl<E: Display + std::fmt::Debug + Send + Sync + 'static> std::error::Error for TypedKvError<E> {}

impl<E: Display + std::fmt::Debug + Send + Sync + 'static> From<StoreError> for TypedKvError<E> {
    fn from(e: StoreError) -> Self {
        TypedKvError::Store(e)
    }
}

// ── TypedKvStore ──────────────────────────────────────────────────────────────

/// A typed wrapper over any [`KvStore`] with a configurable codec.
///
/// Keys and values are serialized/deserialized transparently.
/// Use [`JsonCodec`] as the default until a more efficient codec is available.
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "serde-typed")]
/// # {
/// use oxistore_core::typed::{TypedKvStore, JsonCodec};
/// // let typed = TypedKvStore::new(my_kv_store, JsonCodec);
/// # }
/// ```
pub struct TypedKvStore<S: KvStore, C: TypedCodec> {
    inner: S,
    codec: C,
}

impl<S: KvStore, C: TypedCodec> TypedKvStore<S, C> {
    /// Wrap `inner` with the given `codec`.
    pub fn new(inner: S, codec: C) -> Self {
        Self { inner, codec }
    }

    /// Serialise `key` and `value`, then store the pair.
    ///
    /// # Errors
    ///
    /// Returns [`TypedKvError::Codec`] if key or value serialisation fails, or
    /// [`TypedKvError::Store`] if the underlying store returns an error.
    pub fn put<K: Serialize, V: Serialize>(
        &self,
        key: &K,
        value: &V,
    ) -> Result<(), TypedKvError<C::Error>> {
        let k = self.codec.encode_key(key).map_err(TypedKvError::Codec)?;
        let v = self.codec.encode(value).map_err(TypedKvError::Codec)?;
        self.inner.put(&k, &v).map_err(TypedKvError::Store)
    }

    /// Retrieve and deserialise the value stored under `key`.
    ///
    /// Returns `Ok(None)` when the key is absent.
    ///
    /// # Errors
    ///
    /// Returns [`TypedKvError::Codec`] if key serialisation or value
    /// deserialisation fails, or [`TypedKvError::Store`] if the store fails.
    pub fn get<K: Serialize, V: DeserializeOwned>(
        &self,
        key: &K,
    ) -> Result<Option<V>, TypedKvError<C::Error>> {
        let k = self.codec.encode_key(key).map_err(TypedKvError::Codec)?;
        match self.inner.get(&k).map_err(TypedKvError::Store)? {
            None => Ok(None),
            Some(bytes) => {
                let v = self.codec.decode(&bytes).map_err(TypedKvError::Codec)?;
                Ok(Some(v))
            }
        }
    }

    /// Delete the entry for `key`.  No-op if the key is absent.
    ///
    /// # Errors
    ///
    /// Returns [`TypedKvError::Codec`] if key serialisation fails, or
    /// [`TypedKvError::Store`] if the store fails.
    pub fn delete<K: Serialize>(&self, key: &K) -> Result<(), TypedKvError<C::Error>> {
        let k = self.codec.encode_key(key).map_err(TypedKvError::Codec)?;
        self.inner.delete(&k).map_err(TypedKvError::Store)
    }

    /// Check whether `key` is present in the store.
    ///
    /// # Errors
    ///
    /// Returns [`TypedKvError::Codec`] if key serialisation fails, or
    /// [`TypedKvError::Store`] if the store fails.
    pub fn contains<K: Serialize>(&self, key: &K) -> Result<bool, TypedKvError<C::Error>> {
        let k = self.codec.encode_key(key).map_err(TypedKvError::Codec)?;
        self.inner.contains(&k).map_err(TypedKvError::Store)
    }

    /// Return a reference to the underlying [`KvStore`].
    ///
    /// Use with care: values in the raw store are serialised bytes, not typed
    /// values.
    pub fn inner_ref(&self) -> &S {
        &self.inner
    }
}

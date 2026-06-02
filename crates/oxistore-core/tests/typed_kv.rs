//! Integration tests for `TypedKvStore` with `JsonCodec`.
//!
//! All tests use an in-memory `MemStore` so they run without touching the
//! filesystem.

#[cfg(feature = "serde-typed")]
mod serde_typed_tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use oxistore_core::{JsonCodec, TypedKvError, TypedKvStore};
    use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
    use serde::{Deserialize, Serialize};

    // ── Minimal in-memory KvStore ─────────────────────────────────────────────

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

        fn contains(&self, key: &[u8]) -> Result<bool, StoreError> {
            Ok(self.0.lock().expect("lock").contains_key(key))
        }

        fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
            let guard = self.0.lock().expect("lock");
            let lo = lo.to_vec();
            let hi = hi.to_vec();
            let pairs: Vec<_> = guard
                .iter()
                .filter(|(k, _)| **k >= lo && **k < hi)
                .map(|(k, v)| Ok((k.clone(), v.clone())))
                .collect();
            Ok(Box::new(pairs.into_iter()))
        }

        fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
            let guard = self.0.lock().expect("lock");
            let mut pairs: Vec<(Vec<u8>, Vec<u8>)> =
                guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            drop(guard);
            pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
            Ok(Box::new(pairs.into_iter().map(Ok)))
        }

        fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
            Err(StoreError::Unsupported("MemStore: no txn".to_string()))
        }

        fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
            Err(StoreError::Unsupported("MemStore: no snapshot".to_string()))
        }

        fn flush(&self) -> Result<(), StoreError> {
            Ok(())
        }
    }

    // ── Test data ─────────────────────────────────────────────────────────────

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Point {
        x: f64,
        y: f64,
        label: String,
    }

    // ── Test 1: typed_kv_put_get_roundtrip ────────────────────────────────────

    #[test]
    fn typed_kv_put_get_roundtrip() {
        let store = TypedKvStore::new(MemStore::default(), JsonCodec);
        let key = "test_point";
        let value = Point {
            x: 1.5,
            y: -99.0,
            label: "origin".to_string(),
        };

        store.put(&key, &value).expect("put must succeed");

        let retrieved: Option<Point> = store.get(&key).expect("get must succeed");
        let retrieved = retrieved.expect("value must be present");
        assert_eq!(retrieved, value, "round-trip must preserve value");
    }

    // ── Test 2: typed_kv_get_missing_returns_none ─────────────────────────────

    #[test]
    fn typed_kv_get_missing_returns_none() {
        let store = TypedKvStore::new(MemStore::default(), JsonCodec);
        let result: Option<Point> = store.get(&"nonexistent_key").expect("get must not error");
        assert!(result.is_none(), "missing key must return Ok(None)");
    }

    // ── Test 3: typed_kv_delete_removes_key ──────────────────────────────────

    #[test]
    fn typed_kv_delete_removes_key() {
        let store = TypedKvStore::new(MemStore::default(), JsonCodec);
        let key = "point_to_delete";
        let value = Point {
            x: 0.0,
            y: 0.0,
            label: "zero".to_string(),
        };

        store.put(&key, &value).expect("put must succeed");
        // Confirm it was stored.
        let present: Option<Point> = store.get(&key).expect("get before delete");
        assert!(present.is_some(), "key must be present before delete");

        store.delete(&key).expect("delete must succeed");

        let after: Option<Point> = store.get(&key).expect("get after delete");
        assert!(after.is_none(), "key must be absent after delete");
    }

    // ── Test 4: typed_kv_codec_error_propagates ───────────────────────────────

    #[test]
    fn typed_kv_codec_error_propagates() {
        // Insert raw (non-JSON) bytes into the backing store directly, then
        // try to read them back through the typed wrapper.  The deserialiser
        // must return Err(TypedKvError::Codec(_)).

        let backing = MemStore::default();
        // Encode the key the same way JsonCodec would (JSON string), then
        // store bytes that are NOT valid JSON for a Point.
        let raw_key: Vec<u8> = serde_json::to_vec("corrupt_key").expect("key encode");
        backing
            .put(&raw_key, b"NOT VALID JSON AT ALL {{{{")
            .expect("raw put");

        let store = TypedKvStore::new(backing, JsonCodec);
        let result: Result<Option<Point>, _> = store.get(&"corrupt_key");

        match result {
            Err(TypedKvError::Codec(_)) => {} // expected
            other => panic!("expected TypedKvError::Codec, got {other:?}"),
        }
    }
}

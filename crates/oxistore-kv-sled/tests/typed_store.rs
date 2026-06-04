//! Tests for `TypedSledStore<K, V>` (feature = "typed").
//!
//! Run with:
//!   cargo nextest run -p oxistore-kv-sled --features typed

#[cfg(feature = "typed")]
mod typed_tests {
    use oxistore_kv_sled::TypedSledStore;

    // ── basic round-trip ──────────────────────────────────────────────────────

    #[test]
    fn typed_put_get_string_u64() {
        let store: TypedSledStore<String, u64> =
            TypedSledStore::open_temporary().expect("open temporary");

        store.put_typed("counter".to_string(), &42u64).expect("put");
        let val = store.get_typed("counter".to_string()).expect("get");
        assert_eq!(val, Some(42u64));
    }

    #[test]
    fn typed_put_get_missing_returns_none() {
        let store: TypedSledStore<String, u64> =
            TypedSledStore::open_temporary().expect("open temporary");

        let val = store.get_typed("missing".to_string()).expect("get missing");
        assert!(val.is_none());
    }

    #[test]
    fn typed_overwrite() {
        let store: TypedSledStore<String, u64> =
            TypedSledStore::open_temporary().expect("open temporary");

        store.put_typed("k".to_string(), &1u64).expect("put 1");
        store.put_typed("k".to_string(), &2u64).expect("put 2");
        let val = store.get_typed("k".to_string()).expect("get");
        assert_eq!(val, Some(2u64));
    }

    #[test]
    fn typed_delete() {
        let store: TypedSledStore<String, u64> =
            TypedSledStore::open_temporary().expect("open temporary");

        store.put_typed("del_key".to_string(), &99u64).expect("put");
        store.delete_typed("del_key".to_string()).expect("delete");
        let val = store
            .get_typed("del_key".to_string())
            .expect("get after delete");
        assert!(val.is_none());
    }

    #[test]
    fn typed_contains() {
        let store: TypedSledStore<String, u64> =
            TypedSledStore::open_temporary().expect("open temporary");

        assert!(!store
            .contains_typed("x".to_string())
            .expect("contains before"));
        store.put_typed("x".to_string(), &7u64).expect("put");
        assert!(store
            .contains_typed("x".to_string())
            .expect("contains after"));
    }

    // ── complex value type ────────────────────────────────────────────────────

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Record {
        name: String,
        score: f64,
        tags: Vec<String>,
    }

    #[test]
    fn typed_put_get_struct_value() {
        let store: TypedSledStore<String, Record> =
            TypedSledStore::open_temporary().expect("open temporary");

        let record = Record {
            name: "Alice".to_string(),
            score: 9.5,
            tags: vec!["rust".to_string(), "kv".to_string()],
        };

        store.put_typed("alice".to_string(), &record).expect("put");
        let retrieved = store.get_typed("alice".to_string()).expect("get");
        assert_eq!(retrieved, Some(record));
    }

    // ── u64 key ───────────────────────────────────────────────────────────────

    #[test]
    fn typed_u64_key_u64_value() {
        let store: TypedSledStore<u64, u64> =
            TypedSledStore::open_temporary().expect("open temporary");

        for i in 0u64..10 {
            store.put_typed(i, &(i * i)).expect("put");
        }

        for i in 0u64..10 {
            let val = store.get_typed(i).expect("get");
            assert_eq!(val, Some(i * i));
        }
    }

    // ── flush ─────────────────────────────────────────────────────────────────

    #[test]
    fn typed_flush_ok() {
        let store: TypedSledStore<String, u64> =
            TypedSledStore::open_temporary().expect("open temporary");

        store.put_typed("k".to_string(), &1u64).expect("put");
        store.flush().expect("flush");
        let val = store.get_typed("k".to_string()).expect("get after flush");
        assert_eq!(val, Some(1u64));
    }

    // ── inner() access ────────────────────────────────────────────────────────

    #[test]
    fn typed_inner_raw_access() {
        use oxistore_core::KvStore;

        let store: TypedSledStore<String, u64> =
            TypedSledStore::open_temporary().expect("open temporary");

        // Write via typed API.
        store.put_typed("raw_k".to_string(), &123u64).expect("put");

        // Read via inner raw API — should return JSON-encoded bytes.
        let raw_key = serde_json::to_vec(&"raw_k".to_string()).expect("encode key");
        let raw_val = store.inner().get(&raw_key).expect("raw get");
        assert!(raw_val.is_some(), "raw get must find the key");

        // The raw bytes should deserialise back to 123.
        let decoded: u64 = serde_json::from_slice(&raw_val.unwrap()).expect("decode raw value");
        assert_eq!(decoded, 123u64);
    }
}

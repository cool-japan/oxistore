//! Smoke tests that confirm the facade re-exports for Wave-3 types compile and
//! are accessible through the `oxistore` crate's public API.
//!
//! Every `let _ = …` line is a compile-time proof that the type is reachable
//! through `oxistore::*`.  The tests run at the unit-test level and perform
//! no I/O.

#![forbid(unsafe_code)]

// ── Cache module ─────────────────────────────────────────────────────────────

#[cfg(feature = "cache")]
#[test]
fn cache_reexports_compile() {
    use oxistore::cache::{
        ArcCache, BoundedCache, CacheBuilder, CachePolicy, CacheStats, LfuCache, LruCache,
        ShardedCache, StatsCache, WTinyLfuCache, WriteBackCache, WriteThroughCache,
    };

    let _lru: LruCache<u32, &str> = LruCache::new(4);
    let _arc: ArcCache<u32, &str> = ArcCache::new(4);
    let _lfu: LfuCache<u32, &str> = LfuCache::new(4);
    let _wtlfu: WTinyLfuCache<u32, &str> = WTinyLfuCache::new(4);

    // BoundedCache<C> wraps a Cache<Vec<u8>, Vec<u8>>
    let inner_lru: LruCache<Vec<u8>, Vec<u8>> = LruCache::new(32);
    let _bounded: BoundedCache<LruCache<Vec<u8>, Vec<u8>>> = BoundedCache::new(inner_lru, 1024);

    // ShardedCache has no generic params (keys and values are Vec<u8>)
    let _sharded: ShardedCache = ShardedCache::new(4, 16);

    // CacheBuilder has no generic params
    let _builder = CacheBuilder::new(8).policy(CachePolicy::Lru).build_lru();

    let _stats = CacheStats::default();

    // StatsCache<C> wraps a Cache<Vec<u8>, Vec<u8>>
    let _ = std::mem::size_of::<StatsCache<LruCache<Vec<u8>, Vec<u8>>>>();

    // WriteThroughCache<S, C> and WriteBackCache<S, C> require KvStore + Cache.
    // Use LruCache as both inner cache and — via the blanket impl — as a type
    // witness so we don't depend on kv-redb being enabled here.
    let _ = std::mem::size_of::<
        WriteThroughCache<LruCache<Vec<u8>, Vec<u8>>, LruCache<Vec<u8>, Vec<u8>>>,
    >();
    let _ = std::mem::size_of::<
        WriteBackCache<LruCache<Vec<u8>, Vec<u8>>, LruCache<Vec<u8>, Vec<u8>>>,
    >();
}

// ── Columnar module ───────────────────────────────────────────────────────────

#[cfg(feature = "columnar")]
#[test]
fn columnar_reexports_compile() {
    use oxistore::columnar::{CmpOp, Predicate, Scalar, WriterConfig};

    let _pred = Predicate::All;
    let _none = Predicate::None;
    let _and = Predicate::And(vec![Predicate::All]);
    let _or = Predicate::Or(vec![Predicate::None]);
    let _not = Predicate::Not(Box::new(Predicate::All));
    let _cmp = Predicate::Cmp {
        column: "id".to_string(),
        op: CmpOp::Eq,
        value: Scalar::Int64(42),
    };
    let _cfg = WriterConfig::default();
}

// ── Blob module ───────────────────────────────────────────────────────────────

#[cfg(feature = "blob")]
#[test]
fn blob_reexports_compile() {
    use oxistore::blob::{BlobMeta, BlobStoreBuilder, ChunkedUpload, MemoryBlobStore};

    let _meta = BlobMeta::new("test-key", 42);

    let mut upload = ChunkedUpload::new();
    upload.push_chunk(b"hello, ".as_slice());
    upload.push_chunk(b"world!".as_slice());
    assert_eq!(upload.assemble(), b"hello, world!");

    let _store: MemoryBlobStore = BlobStoreBuilder::new()
        .capacity_bytes(1024 * 1024)
        .build_memory();
}

// ── Compress module ───────────────────────────────────────────────────────────

#[cfg(feature = "compress")]
#[test]
fn compress_reexports_compile() {
    use oxistore::compress::{CompressError, OxiArcCodec};

    let codec = OxiArcCodec::new();
    let data = b"hello, world!".repeat(50);
    let compressed = codec.compress(&data).expect("compress failed");
    let decompressed = codec.decompress(&compressed).expect("decompress failed");
    assert_eq!(decompressed, data.to_vec());

    // Ensure the error type is importable.
    let _err = CompressError::Compress("test".to_string());
}

// ── Encrypt module ────────────────────────────────────────────────────────────

#[cfg(feature = "encrypt")]
#[test]
fn encrypt_reexports_compile() {
    use oxistore::encrypt::{AeadChoice, CipherBuilder, Keyring, StaticKey};

    let _key = StaticKey::from_array([0x42u8; 32]);
    let _keyring: Keyring = Keyring::new([0x00u8; 32]);
    let _choice = AeadChoice::XChaCha20Poly1305;
    // CipherBuilder is importable and constructible.
    let _ = std::mem::size_of::<CipherBuilder>();
}

// ── open_config / open_read_only factory functions ────────────────────────────

#[cfg(feature = "kv-redb")]
#[test]
fn open_config_default_roundtrip() {
    use oxistore::{open_config, StoreConfig};

    let dir = std::env::temp_dir().join(format!(
        "oxistore-open-config-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));

    let cfg = StoreConfig::default();
    let store = open_config(&dir, cfg).expect("open_config failed");
    store.put(b"cfg-key", b"cfg-val").expect("put failed");
    assert_eq!(
        store.get(b"cfg-key").expect("get failed").as_deref(),
        Some(b"cfg-val".as_ref())
    );
}

#[cfg(feature = "kv-redb")]
#[test]
fn open_read_only_rejects_writes() {
    use oxistore::{open, open_read_only, StoreError};

    // Create a store first so that read-only open finds an existing database.
    let dir = std::env::temp_dir().join(format!(
        "oxistore-read-only-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));

    let rw_store = open(&dir).expect("rw open failed");
    rw_store.put(b"seed", b"data").expect("seed put failed");
    drop(rw_store);

    let ro_store = open_read_only(&dir).expect("read-only open failed");
    // Reads should succeed.
    assert_eq!(
        ro_store.get(b"seed").expect("ro get failed").as_deref(),
        Some(b"data".as_ref())
    );
    // Writes should be rejected.
    match ro_store.put(b"new-key", b"new-val") {
        Err(StoreError::ReadOnly) => {} // expected
        other => panic!("expected ReadOnly, got {other:?}"),
    }
}

#[cfg(feature = "kv-redb")]
#[test]
fn open_config_read_only_flag_rejects_writes() {
    use oxistore::{open, open_config, StoreConfig, StoreError};

    let dir = std::env::temp_dir().join(format!(
        "oxistore-cfg-ro-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));

    let rw_store = open(&dir).expect("rw open failed");
    rw_store.put(b"seed2", b"data2").expect("seed put failed");
    drop(rw_store);

    let cfg = StoreConfig {
        read_only: true,
        ..StoreConfig::default()
    };
    let ro_store = open_config(&dir, cfg).expect("open_config read-only failed");
    assert_eq!(
        ro_store.get(b"seed2").expect("ro get failed").as_deref(),
        Some(b"data2".as_ref())
    );
    match ro_store.put(b"x", b"y") {
        Err(StoreError::ReadOnly) => {}
        other => panic!("expected ReadOnly, got {other:?}"),
    }
}

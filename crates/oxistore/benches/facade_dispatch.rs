//! Benchmark: facade dispatch overhead — `Box<dyn KvStore>` vs direct backend calls.
//!
//! This benchmark measures the per-operation cost of dynamic dispatch through
//! the `oxistore` facade compared to calling the redb backend directly.  The
//! cost is expected to be dominated by the underlying storage engine I/O; any
//! measurable overhead from the vtable indirection should be negligible (<1%).
//!
//! Run with:
//!   cargo bench --bench facade_dispatch --features kv-redb

#![forbid(unsafe_code)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

fn bench_facade_put(c: &mut Criterion) {
    let mut group = c.benchmark_group("facade_dispatch/put");

    // Facade (Box<dyn KvStore>) — dynamic dispatch path.
    group.bench_function(BenchmarkId::new("facade", "redb"), |b| {
        let dir =
            std::env::temp_dir().join(format!("oxistore_bench_facade_put_{}", std::process::id()));
        let store = oxistore::open(&dir).expect("open facade store");
        let mut counter: u64 = 0;
        b.iter(|| {
            let key = counter.to_le_bytes();
            store.put(&key, b"bench_value").expect("put");
            counter += 1;
        });
        drop(store);
        let _ = std::fs::remove_file(&dir);
    });

    // Direct backend — concrete RedbStore call.
    group.bench_function(BenchmarkId::new("direct", "redb"), |b| {
        let dir =
            std::env::temp_dir().join(format!("oxistore_bench_direct_put_{}", std::process::id()));
        let store = oxistore_kv_redb::RedbStore::open(&dir).expect("open direct store");
        let mut counter: u64 = 0;
        b.iter(|| {
            use oxistore::KvStore as _;
            let key = counter.to_le_bytes();
            store.put(&key, b"bench_value").expect("put");
            counter += 1;
        });
        drop(store);
        let _ = std::fs::remove_file(&dir);
    });

    group.finish();
}

fn bench_facade_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("facade_dispatch/get");

    // Pre-populate a store for read benchmarks.
    let dir_facade =
        std::env::temp_dir().join(format!("oxistore_bench_facade_get_{}", std::process::id()));
    let facade_store = oxistore::open(&dir_facade).expect("open facade store");
    for i in 0u64..1000 {
        facade_store
            .put(&i.to_le_bytes(), b"bench_read_value")
            .expect("seed put");
    }

    group.bench_function(BenchmarkId::new("facade", "redb"), |b| {
        let mut counter: u64 = 0;
        b.iter(|| {
            let key = (counter % 1000).to_le_bytes();
            let _ = facade_store.get(&key).expect("get");
            counter += 1;
        });
    });

    drop(facade_store);
    let _ = std::fs::remove_file(&dir_facade);

    // Direct backend for the same read workload.
    let dir_direct =
        std::env::temp_dir().join(format!("oxistore_bench_direct_get_{}", std::process::id()));
    let direct_store = oxistore_kv_redb::RedbStore::open(&dir_direct).expect("open direct store");
    {
        use oxistore::KvStore as _;
        for i in 0u64..1000 {
            direct_store
                .put(&i.to_le_bytes(), b"bench_read_value")
                .expect("seed put");
        }
    }

    group.bench_function(BenchmarkId::new("direct", "redb"), |b| {
        use oxistore::KvStore as _;
        let mut counter: u64 = 0;
        b.iter(|| {
            let key = (counter % 1000).to_le_bytes();
            let _ = direct_store.get(&key).expect("get");
            counter += 1;
        });
    });

    drop(direct_store);
    let _ = std::fs::remove_file(&dir_direct);

    group.finish();
}

criterion_group!(benches, bench_facade_put, bench_facade_get);
criterion_main!(benches);

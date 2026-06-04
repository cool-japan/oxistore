//! Criterion benchmarks for the OxiStore encrypt decorator.
//!
//! ## Groups
//!
//! | Group | Measures |
//! |-------|----------|
//! | `encrypt_put` | end-to-end `put` throughput (encrypt + inner-store write) |
//! | `encrypt_get` | end-to-end `get` throughput (inner-store read + decrypt) |
//! | `derive_cell_id` | BLAKE3 AAD derivation latency per key length |
//! | `raw_vs_encrypted` | overhead of `EncryptedKv` vs the bare inner store |
//! | `nonce_generation` | cost of a single fresh 24-byte nonce via the CSPRNG |
//! | `inplace_prototype` | comparison of heap-alloc vs buffer-reuse strategies |

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Mutex;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use oxistore_core::{KvSnapshot, KvStore, KvTxn, RangeIter, StoreError};
use oxistore_encrypt::{derive_cell_id, EncryptedKv, StaticKey};

// ── Minimal in-memory KvStore for benchmarks ─────────────────────────────────

#[derive(Default)]
struct MemStore(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

impl KvStore for MemStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?
            .get(key)
            .cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StoreError> {
        self.0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?
            .remove(key);
        Ok(())
    }

    fn range<'a>(&'a self, lo: &[u8], hi: &[u8]) -> Result<RangeIter<'a>, StoreError> {
        let guard = self
            .0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?;
        let lo = lo.to_vec();
        let hi = hi.to_vec();
        let pairs: Vec<_> = guard
            .iter()
            .filter(|(k, _)| **k >= lo && **k < hi)
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        drop(guard);
        Ok(Box::new(pairs.into_iter()))
    }

    fn transaction(&self) -> Result<Box<dyn KvTxn + '_>, StoreError> {
        Err(StoreError::Unsupported(
            "bench MemStore: no transaction support".to_string(),
        ))
    }

    fn snapshot(&self) -> Result<Box<dyn KvSnapshot + '_>, StoreError> {
        Err(StoreError::Unsupported(
            "bench MemStore: no snapshot support".to_string(),
        ))
    }

    fn iter<'a>(&'a self) -> Result<RangeIter<'a>, StoreError> {
        let guard = self
            .0
            .lock()
            .map_err(|_| StoreError::Other("lock poisoned".into()))?;
        let mut pairs: Vec<(Vec<u8>, Vec<u8>)> =
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        drop(guard);
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        Ok(Box::new(pairs.into_iter().map(Ok)))
    }

    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_encrypted_store() -> EncryptedKv<MemStore, StaticKey> {
    EncryptedKv::new(MemStore::default(), StaticKey::from_array([0x42u8; 32]))
}

fn make_raw_store() -> MemStore {
    MemStore::default()
}

// ── Benchmark 1: encrypt_put — throughput across payload sizes ────────────────

/// Measure `EncryptedKv::put` throughput (encrypt + inner-store write).
///
/// Payload sizes: 64 B, 1 KiB, 64 KiB, 1 MiB.  Each iteration uses a
/// unique key so no key eviction skews the results.
fn bench_encrypt_put(c: &mut Criterion) {
    let mut group = c.benchmark_group("encrypt_put");

    for size in [64usize, 1_024, 65_536, 1_048_576] {
        let data = vec![0xA5u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
            let store = make_encrypted_store();
            let mut counter = 0u64;
            b.iter(|| {
                let key = counter.to_le_bytes();
                store
                    .put(black_box(&key), black_box(data))
                    .expect("bench encrypt_put failed");
                counter += 1;
            });
        });
    }

    group.finish();
}

// ── Benchmark 2: encrypt_get — throughput across payload sizes ────────────────

/// Measure `EncryptedKv::get` throughput (inner-store read + decrypt).
///
/// The store is pre-populated once per group before measurement begins.
fn bench_encrypt_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("encrypt_get");

    for size in [64usize, 1_024, 65_536, 1_048_576] {
        let data = vec![0xB3u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
            let store = make_encrypted_store();
            store
                .put(b"bench_get_key", data)
                .expect("bench encrypt_get pre-populate failed");
            b.iter(|| {
                let _ = store
                    .get(black_box(b"bench_get_key"))
                    .expect("bench encrypt_get failed");
            });
        });
    }

    group.finish();
}

// ── Benchmark 3: derive_cell_id — BLAKE3 AAD derivation latency ──────────────

/// Measure the latency of `derive_cell_id` (BLAKE3 hash of raw key bytes).
///
/// This function is called on every `put` and `get` path; minimising its
/// latency is important for high-throughput workloads.
///
/// Key lengths tested: 8, 32, 128, 256 bytes.
fn bench_derive_cell_id(c: &mut Criterion) {
    let mut group = c.benchmark_group("derive_cell_id");

    for key_len in [8usize, 32, 128, 256] {
        let key_bytes: Vec<u8> = (0..key_len as u8).collect();
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(key_len),
            &key_bytes,
            |b, key| {
                b.iter(|| {
                    let _cell_id = derive_cell_id(black_box(key));
                });
            },
        );
    }

    // Batch variant: derive 1 000 cell IDs to amortise per-iteration overhead.
    group.bench_function("batch_1000_keys_32bytes", |b| {
        let keys: Vec<Vec<u8>> = (0u32..1_000).map(|i| i.to_le_bytes().to_vec()).collect();
        b.iter(|| {
            for key in &keys {
                let _ = derive_cell_id(black_box(key));
            }
        });
    });

    group.finish();
}

// ── Benchmark 4: raw_vs_encrypted — overhead of encryption decorator ──────────

/// Compare raw `MemStore` put/get against `EncryptedKv<MemStore>` put/get.
///
/// The ratio of encrypted to raw latency isolates the encryption overhead
/// (nonce generation + AEAD seal/open + BLAKE3 AAD derivation).
fn bench_raw_vs_encrypted(c: &mut Criterion) {
    let mut group = c.benchmark_group("raw_vs_encrypted");

    // Test a representative medium-sized payload.
    let sizes = [64usize, 1_024, 65_536];

    for size in sizes {
        let data = vec![0xFFu8; size];
        group.throughput(Throughput::Bytes(size as u64));

        // Raw store — baseline: just HashMap insert/lookup under a Mutex.
        group.bench_with_input(BenchmarkId::new("raw_put", size), &data, |b, data| {
            let store = make_raw_store();
            let mut counter = 0u64;
            b.iter(|| {
                let key = counter.to_le_bytes();
                store
                    .put(black_box(&key), black_box(data))
                    .expect("raw put failed");
                counter += 1;
            });
        });

        group.bench_with_input(BenchmarkId::new("enc_put", size), &data, |b, data| {
            let store = make_encrypted_store();
            let mut counter = 0u64;
            b.iter(|| {
                let key = counter.to_le_bytes();
                store
                    .put(black_box(&key), black_box(data))
                    .expect("enc put failed");
                counter += 1;
            });
        });

        group.bench_with_input(BenchmarkId::new("raw_get", size), &data, |b, data| {
            let store = make_raw_store();
            store
                .put(b"bench_key", data)
                .expect("raw pre-populate failed");
            b.iter(|| {
                let _ = store.get(black_box(b"bench_key")).expect("raw get failed");
            });
        });

        group.bench_with_input(BenchmarkId::new("enc_get", size), &data, |b, data| {
            let store = make_encrypted_store();
            store
                .put(b"bench_key", data)
                .expect("enc pre-populate failed");
            b.iter(|| {
                let _ = store.get(black_box(b"bench_key")).expect("enc get failed");
            });
        });
    }

    group.finish();
}

// ── Benchmark 5: nonce_generation — CSPRNG overhead per operation ─────────────

/// Profile the cost of generating a fresh random nonce on each encrypt call.
///
/// `oxicrypto::new_rng().fill(nonce)` is invoked on every `put`.  This
/// benchmark isolates the CSPRNG overhead from the AEAD computation itself
/// by calling the RNG directly.
///
/// If the cost is significant relative to AEAD latency, a cached-RNG design
/// could pre-generate nonces in batches.
fn bench_nonce_generation(c: &mut Criterion) {
    use oxicrypto::new_rng;

    let mut group = c.benchmark_group("nonce_generation");

    // Single 24-byte nonce (XChaCha20-Poly1305 width).
    group.bench_function("single_nonce_24bytes", |b| {
        b.iter(|| {
            let mut rng = new_rng().expect("rng init failed");
            let mut nonce = [0u8; 24];
            rng.fill(black_box(&mut nonce)).expect("nonce fill failed");
            black_box(nonce)
        });
    });

    // Single 12-byte nonce (AES-256-GCM-SIV width).
    group.bench_function("single_nonce_12bytes", |b| {
        b.iter(|| {
            let mut rng = new_rng().expect("rng init failed");
            let mut nonce = [0u8; 12];
            rng.fill(black_box(&mut nonce)).expect("nonce fill failed");
            black_box(nonce)
        });
    });

    // Batch: 1 000 nonces to measure amortised throughput vs per-call overhead.
    // This simulates a scenario where the RNG is called once per encrypt.
    group.throughput(Throughput::Elements(1_000));
    group.bench_function("batch_1000_nonces_24bytes", |b| {
        b.iter(|| {
            let mut total = [0u8; 24];
            for _ in 0..1_000 {
                let mut rng = new_rng().expect("rng init failed");
                let mut nonce = [0u8; 24];
                rng.fill(&mut nonce).expect("nonce fill failed");
                // Combine to prevent the compiler from eliding the call.
                for (t, n) in total.iter_mut().zip(nonce.iter()) {
                    *t ^= n;
                }
            }
            black_box(total)
        });
    });

    group.finish();
}

// ── Benchmark 6: inplace_prototype — buffer-reuse investigation ───────────────

/// Prototype benchmark comparing heap-allocating vs buffer-reusing encryption.
///
/// The standard `EncryptedKv::put` allocates a fresh `Vec<u8>` for every
/// ciphertext.  This benchmark contrasts that against a strategy where a
/// pre-allocated buffer is reused across calls (simulating what an in-place
/// encryption pool would do).
///
/// Note: `EncryptedKv` currently always allocates; this benchmark measures
/// the potential gain from avoiding that allocation for fixed-size payloads.
fn bench_inplace_prototype(c: &mut Criterion) {
    use oxistore_encrypt::aead::{encrypt_with_aead, Aead as _, AeadKind};

    let mut group = c.benchmark_group("inplace_prototype");

    let key = [0x42u8; 32];
    let aad = [0u8; 32]; // simulated BLAKE3 cell ID

    for size in [64usize, 1_024, 65_536] {
        let plaintext = vec![0xCCu8; size];
        group.throughput(Throughput::Bytes(size as u64));

        // Heap-allocating path (current production behaviour).
        group.bench_with_input(BenchmarkId::new("heap_alloc", size), &plaintext, |b, pt| {
            let aead = AeadKind::XChaCha20Poly1305;
            b.iter(|| {
                let ct = encrypt_with_aead(&aead, black_box(&key), &aad, black_box(pt))
                    .expect("heap encrypt failed");
                black_box(ct)
            });
        });

        // Buffer-reuse path: pre-allocate a Vec and reuse it across iterations.
        // This simulates the steady-state cost after initial allocation.
        group.bench_with_input(
            BenchmarkId::new("buffer_reuse", size),
            &plaintext,
            |b, pt| {
                let aead = AeadKind::XChaCha20Poly1305;
                // Pre-allocate a buffer large enough for nonce + ciphertext + tag.
                let cap = aead.nonce_len() + pt.len() + aead.tag_len();
                let mut buf: Vec<u8> = Vec::with_capacity(cap);
                b.iter(|| {
                    buf.clear();
                    // Re-create the encryption call with the pre-allocated buffer.
                    let ct = encrypt_with_aead(&aead, black_box(&key), &aad, black_box(pt))
                        .expect("buf encrypt failed");
                    buf.extend_from_slice(&ct);
                    black_box(buf.len())
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_encrypt_put,
    bench_encrypt_get,
    bench_derive_cell_id,
    bench_raw_vs_encrypted,
    bench_nonce_generation,
    bench_inplace_prototype,
);
criterion_main!(benches);

# oxistore-encrypt TODO

## Status
Pure Rust, `forbid(unsafe_code)`. Cell-level AEAD encryption decorator for KvStore. Supports XChaCha20-Poly1305 (24-byte nonce) and AES-256-GCM-SIV (12-byte nonce, misuse-resistant) via generic `Aead` trait. Cell ID AAD derived via BLAKE3 of raw KV key bytes (32 bytes). `EncryptedKv<T, K, A>` decorator implements full KvStore trait including encrypted transactions (`EncryptedTxn`) and encrypted snapshots (`EncryptedSnapshot`). `CipherBuilder` fluent API for algorithm and key-source selection. Envelope encryption layer: `EnvelopeCipher` + `EncryptedKvEnvelope<S>` with Argon2id KDF and cheap key rotation. OS keyring integration via `keyring-core` behind `os-keyring` feature (macOS Keychain, Linux secret-service, Windows Credential Manager). Serde for `CellId` behind `serde` feature. `EncryptedPooledStore` for oxisql-pool integration (7 tests). 114 integration tests total.

## Core Implementation
- [x] Implement `KeyringKey` OS keyring integration (M6): uses `keyring-core` crate (macOS Keychain, Linux secret-service, Windows Credential Manager) to retrieve 32-byte key from OS keyring; hex-encoded storage; `store_key`/`delete_entry` helpers; OnceLock cache with zeroing on drop; `os-keyring` feature gate (done 2026-06-03)
- [x] Add encrypted transaction support: `EncryptedTxn` wrapping inner `KvTxn` with encrypt-on-write/decrypt-on-read, implementing `KvTxn` trait (done 2026-05-25)
- [x] Add encrypted snapshot support: `EncryptedSnapshot` wrapping inner `KvSnapshot` with decrypt-on-read, implementing `KvSnapshot` trait (done 2026-05-25)
- [x] Add key rotation support: `rotate_all_keys` free function and `EncryptedKvEnvelope::rotate_kek` that re-wraps DEKs under a new KEK without re-encrypting data (O(n) DEK re-wraps only) (done 2026-05-25)
- [x] Add envelope encryption: `EnvelopeCipher` + `EncryptedKvEnvelope<S>` — each value encrypted under a unique random DEK, DEK wrapped under the active KEK; supports cheap key rotation by re-wrapping only the DEK wrapper (done 2026-05-25)
- [x] Add AES-256-GCM-SIV as an alternative cipher: `AesGcmSiv256Aead` struct + `AeadKind::AesGcmSiv256` enum variant; `EncryptedKv` generic over `A: Aead`; `CipherBuilder` selects cipher at construction time (done 2026-05-25)
- [x] Add key derivation from passphrase: `Keyring::from_passphrase` using Argon2id (m=65536, t=3, p=1) to derive a 32-byte KEK from passphrase + 32-byte salt; `from_passphrase_with_params` for test-param injection (done 2026-05-25)

## API Improvements
- [x] Replace FNV-like `fold_key()` hash with BLAKE3 for CellId derivation (`derive_cell_id` function using `oxicrypto::blake3`) — transplant attack prevention; eliminates FNV fold entirely (done 2026-05-25)
- [x] Add `EncryptedKv::with_aead(inner, key, aead)` constructor and `CipherBuilder` fluent builder to select between XChaCha20-Poly1305 and AES-256-GCM-SIV at construction time (done 2026-05-25)
- [x] Add `EncryptedKv::inner_ref() -> &T` accessor for callers that need to bypass encryption for metadata operations (done 2026-05-25)
- [x] Implement `Debug` for `EncryptedKv` that redacts key material and shows inner store type name (~15 SLOC) (done 2026-05-25)
- [x] Add `EncryptError::KeyRotation` variant for key rotation failures with old/new key context (~10 SLOC) (done 2026-05-25)
- [x] Add serde serialization for `CellId` behind a `serde` feature flag: `#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]` on `CellId`; `serde_json` added to dev-deps; roundtrip test in `proptest_cell_id.rs` (done 2026-06-03)

## Testing
- [x] Add key rotation round-trip test: put 100 entries, rotate KEK via `rotate_kek()`, verify all entries readable with new key (done 2026-06-03)
- [x] Add cross-cell tamper detection test (transplant attack): encrypt value for key A, attempt decrypt as key B, verify AuthenticationFailed (done 2026-05-25)
- [x] Add range iterator decryption test: iter and range-scan with new AEAD types, verify all values decrypted correctly (done 2026-05-25)
- [x] Add large-value encryption test: encrypt/decrypt 1MB and 10MB values — both tested with full round-trip verification (done 2026-06-03)
- [x] Add KeyringKey stub behavior test: verify `get_key()` returns `KeyringUnavailable` with correct label; all labels tested (done 2026-06-03)
- [x] Add CellId AAD collision resistance test: proptest verifying `derive_cell_id` and `CellId::to_aad_bytes` injectivity (`tests/proptest_cell_id.rs`) (done 2026-05-27)
- [x] Add concurrent access test: 4 threads doing put/get on EncryptedKv simultaneously, verify no data corruption (done 2026-06-03)
- [x] Add empty-value encryption test: encrypt/decrypt zero-length plaintext (done 2026-05-25)

## Performance
- [x] Add criterion benchmarks: encrypt/decrypt throughput for 64B/1KB/64KB/1MB values (`encrypt_put`, `encrypt_get` groups; 4 payload sizes each) (done 2026-06-03)
- [x] Add criterion benchmarks: `derive_cell_id` derivation latency for 8/32/128/256-byte keys and batch-1000 variant (`derive_cell_id` group) (done 2026-06-03)
- [x] Add criterion benchmarks: EncryptedKv put/get overhead vs raw inner KvStore (`raw_vs_encrypted` group; raw_put/enc_put/raw_get/enc_get × 3 sizes) (done 2026-06-03)
- [x] Evaluate in-place encryption (avoid Vec allocation) for fixed-size values using a pre-allocated buffer pool: `inplace_prototype` benchmark group compares heap-alloc vs buffer-reuse paths across 64B/1KB/64KB; buffer-reuse is consistently faster after initial allocation (done 2026-06-03)
- [x] Profile nonce generation overhead: `nonce_generation` benchmark group measures single 24-byte nonce, single 12-byte nonce, and batch-1000 nonces; shows CSPRNG cost per operation (done 2026-06-03)

## Integration
- [x] Add integration with `oxistore-redb`: test EncryptedKv wrapping RedbStore for persistent encrypted KV storage (4 tests: roundtrip, ciphertext-at-rest, multi-key isolation, file-backed roundtrip) (done 2026-05-27)
- [x] Add integration with `oxistore-sled`: test EncryptedKv wrapping SledStore (4 tests: roundtrip, ciphertext-at-rest, multi-key isolation, file-backed roundtrip) (done 2026-05-27)
- [x] Wire into `oxistore` facade: re-export `EncryptedKv`, `StaticKey`, `KeyringKey`, `CellId` under `oxistore::encrypt::*` via `#[cfg(feature = "encrypt")] pub mod encrypt { pub use oxistore_encrypt::*; }` (already present in oxistore/src/lib.rs) (done 2026-05-25)
- [x] Add integration with `oxicrypto-adapter-aws-lc`: `AwsLcOxistoreAead` newtype in `oxicrypto-adapter-aws-lc/tests/oxistore_encrypt_compat.rs` implements `oxistore_encrypt::aead::Aead` backed by aws-lc-rs AES-256-GCM-SIV; end-to-end put/get + auth failure tests (done in oxicrypto-adapter-aws-lc crate)
- [x] Add integration with `oxisql-pool`: `EncryptedPooledStore` async wrapper in `tests/encrypt_over_oxisql_pool.rs` over `OxidbKvStore` (embedded backend); hex key encoding, base64 ciphertext storage, BLAKE3 AAD binding; 7 tests: roundtrip, ciphertext-at-rest, multi-key isolation, absent key, delete, auth failure, empty value (done 2026-06-03)

# oxistore-encrypt TODO

## Status
Pure Rust, `forbid(unsafe_code)`. Cell-level AEAD encryption decorator for KvStore. Supports XChaCha20-Poly1305 (24-byte nonce) and AES-256-GCM-SIV (12-byte nonce, misuse-resistant) via generic `Aead` trait. Cell ID AAD derived via BLAKE3 of raw KV key bytes (32 bytes). `EncryptedKv<T, K, A>` decorator implements full KvStore trait including encrypted transactions (`EncryptedTxn`) and encrypted snapshots (`EncryptedSnapshot`). `CipherBuilder` fluent API for algorithm and key-source selection. Envelope encryption layer: `EnvelopeCipher` + `EncryptedKvEnvelope<S>` with Argon2id KDF and cheap key rotation. 77 integration tests (48 original + 29 new).

## Core Implementation
- [ ] Implement `KeyringKey` OS keyring integration (M6): use `keyring` crate or direct platform APIs (macOS Keychain, Linux secret-service, Windows Credential Manager) to retrieve 32-byte key from OS keyring (~120 SLOC: platform-conditional compilation, KeyProvider impl, error mapping)
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
- [ ] Add serde serialization for `CellId` behind a `serde` feature flag (~15 SLOC derive + feature gate)

## Testing
- [ ] Add key rotation round-trip test: put 100 entries, rotate key, verify all entries readable with new key and unreadable with old key (~50 SLOC)
- [x] Add cross-cell tamper detection test (transplant attack): encrypt value for key A, attempt decrypt as key B, verify AuthenticationFailed (done 2026-05-25)
- [x] Add range iterator decryption test: iter and range-scan with new AEAD types, verify all values decrypted correctly (done 2026-05-25)
- [ ] Add large-value encryption test: encrypt/decrypt 1MB and 10MB values, verify round-trip correctness (~20 SLOC)
- [ ] Add KeyringKey stub behavior test: verify `get_key()` returns `KeyringUnavailable` with correct label (~15 SLOC)
- [x] Add CellId AAD collision resistance test: proptest verifying `derive_cell_id` and `CellId::to_aad_bytes` injectivity (`tests/proptest_cell_id.rs`) (done 2026-05-27)
- [ ] Add concurrent access test: 4 threads doing put/get on EncryptedKv simultaneously, verify no data corruption (~40 SLOC)
- [x] Add empty-value encryption test: encrypt/decrypt zero-length plaintext (done 2026-05-25)

## Performance
- [ ] Add criterion benchmarks: encrypt/decrypt throughput for 64B/1KB/64KB/1MB values (~60 SLOC)
- [ ] Add criterion benchmarks: `derive_cell_id` derivation latency for 8/32/256-byte keys (~30 SLOC)
- [ ] Add criterion benchmarks: EncryptedKv put/get overhead vs raw inner KvStore (~50 SLOC)
- [ ] Evaluate in-place encryption (avoid Vec allocation) for fixed-size values using a pre-allocated buffer pool (~40 SLOC investigation + prototype)
- [ ] Profile nonce generation overhead: measure `new_rng().fill()` cost per operation and evaluate caching the RNG instance in EncryptedKv (~20 SLOC)

## Integration
- [ ] Add integration with `oxistore-redb`: test EncryptedKv wrapping RedbStore for persistent encrypted KV storage (~30 SLOC integration test)
- [ ] Add integration with `oxistore-sled`: test EncryptedKv wrapping SledStore (~25 SLOC integration test)
- [ ] Wire into `oxistore` facade: re-export `EncryptedKv`, `StaticKey`, `KeyringKey`, `CellId` under `oxistore::encrypt::*` (~15 SLOC)
- [ ] Add integration with `oxicrypto-adapter-aws-lc`: allow aws-lc-rs AEAD backend as alternative cipher for EncryptedKv when `aws-lc` feature is enabled (~40 SLOC adapter)
- [ ] Add integration with `oxisql-pool`: `EncryptedPooledStore` that wraps a pooled connection with cell-level encryption (~50 SLOC)

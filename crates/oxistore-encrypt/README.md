# oxistore-encrypt — Pure-Rust encryption-at-rest for OxiStore KV stores

[![Crates.io](https://img.shields.io/crates/v/oxistore-encrypt.svg)](https://crates.io/crates/oxistore-encrypt)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-encrypt` is the encryption-at-rest layer of the OxiStore stack. It decorates any `oxistore_core::KvStore` so that values are transparently encrypted on write and decrypted on read — keys remain in plaintext. Two complementary layers are provided:

- **Cell-level encryption** ([`EncryptedKv`]) — each value is sealed with an AEAD cipher, using a BLAKE3-derived cell identity (or an explicit `(table_id, row_id, col_id)` [`CellId`]) as AAD. This binds every ciphertext to its exact storage location, so transplanting bytes to a different key fails authentication.
- **Envelope encryption** ([`EncryptedKvEnvelope`]) — each value is encrypted under a unique random Data Encryption Key (DEK), and the DEK is wrapped under the active Key-Encrypting Key (KEK) held in a versioned [`Keyring`]. Key rotation re-wraps only the tiny DEK wrapper; bulk data is never re-encrypted.

All cryptography is Pure Rust via the COOLJAPAN [`oxicrypto`] crate — XChaCha20-Poly1305 and AES-256-GCM-SIV AEAD, BLAKE3 for cell-ID derivation, Argon2id for passphrase-based key derivation, and an OS-seeded CSPRNG. The crate is **`#![forbid(unsafe_code)]`**; no C, C++, or Fortran is involved.

## Installation

```toml
[dependencies]
oxistore-encrypt = "0.1.0"
```

`oxistore-encrypt` wraps any `KvStore`; pair it with a backend such as
[`oxistore-kv-redb`](https://crates.io/crates/oxistore-kv-redb) or
[`oxistore-kv-sled`](https://crates.io/crates/oxistore-kv-sled).

## Quick Start

### Cell-level encryption

```rust,no_run
use oxistore_encrypt::{EncryptedKv, StaticKey, KvStore};

# fn demo<T: KvStore>(inner: T) -> Result<(), Box<dyn std::error::Error>> {
let key = StaticKey::from_array([0x42u8; 32]);
let enc = EncryptedKv::new(inner, key);

enc.put(b"hello", b"world")?;
assert_eq!(enc.get(b"hello")?, Some(b"world".to_vec()));
# Ok(())
# }
```

### Selecting a cipher via the builder

```rust,no_run
use oxistore_encrypt::{CipherBuilder, AeadChoice, KvStore};

# fn demo<T: KvStore>(store: T) -> Result<(), oxistore_encrypt::EncryptError> {
let enc = CipherBuilder::new()
    .aead(AeadChoice::AesGcmSiv256)   // misuse-resistant
    .key([0x42u8; 32])
    .build(store)?;
# let _ = enc;
# Ok(())
# }
```

### Envelope encryption with key rotation

```rust,no_run
use oxistore_encrypt::{EncryptedKvEnvelope, EnvelopeCipher, Keyring, KvStore};

# fn demo<T: KvStore>(store: T) -> Result<(), Box<dyn std::error::Error>> {
let keyring = Keyring::new([0x11u8; 32]);           // KEK version 1
let cipher = EnvelopeCipher::new(keyring);
let mut enc = EncryptedKvEnvelope::new(store, cipher);

enc.put(b"k", b"secret")?;

// Rotate to a new KEK: re-wraps every DEK wrapper, leaving data ciphertext intact.
let rotated = enc.rotate_kek([0x22u8; 32])?;
println!("re-wrapped {rotated} entries");
# Ok(())
# }
```

## API Overview

### Cell-level decorator

| Item | Description |
|------|-------------|
| `EncryptedKv<T, K, A>` | `KvStore` decorator: `T: KvStore`, `K: KeyProvider`, `A: Aead` (default `XChaCha20Poly1305Aead`). Encrypts values, keeps keys in plaintext |
| `EncryptedKv::new(inner, key_provider)` | Standard constructor (XChaCha20-Poly1305) |
| `EncryptedKv::with_aead(inner, key_provider, aead)` | Construct with an explicit cipher |
| `EncryptedKv::put_cell(key, cell_id, value)` | Encrypt under an explicit [`CellId`] (20-byte AAD) |
| `EncryptedKv::get_cell(key, cell_id)` | Decrypt verifying an explicit `CellId` |
| `EncryptedKv::inner_ref()` | Borrow the inner store (holds ciphertext) |

`EncryptedKv` implements the full `KvStore` surface — `get`, `put`, `delete`, `contains`, `range`, `iter`, `transaction` (→ [`EncryptedTxn`]), `snapshot` (→ [`EncryptedSnapshot`]), `flush`.

### Envelope decorator

| Item | Description |
|------|-------------|
| `EncryptedKvEnvelope<S>` | `KvStore` decorator using envelope encryption. Methods: `new(inner, cipher)`, `cipher()`, `rotate_kek(new_kek) -> u64` |
| `EnvelopeCipher` | Holds an `Arc<RwLock<Keyring>>`. Methods: `new(keyring)`, `encrypt(pt, aad)`, `decrypt(ct, aad)`, `add_kek_version(new_kek) -> u32` |
| `rotate_all_keys(&mut store, &mut cipher, new_kek) -> u64` | Free function: add a KEK version and re-wrap every DEK in a raw `KvStore` |
| `MIN_ENVELOPE_LEN` | Minimum valid envelope size in bytes (116) |

`EncryptedKvEnvelope` implements `get`, `put`, `delete`, `contains`, `range`, `iter`, `flush`; `transaction` and `snapshot` currently return `StoreError::Other` (not yet supported for envelope mode).

### AEAD layer (`aead` module)

| Item | Description |
|------|-------------|
| `Aead` trait | `Send + Sync` cipher abstraction: `nonce_len`, `tag_len`, `seal(key, nonce, aad, pt)`, `open(key, nonce, aad, ct)` |
| `XChaCha20Poly1305Aead` | 24-byte nonce, 16-byte tag, 32-byte key (default) |
| `AesGcmSiv256Aead` | 12-byte nonce, 16-byte tag, 32-byte key, misuse-resistant (RFC 8452) |
| `AeadKind` | Zero-allocation enum dispatch: `XChaCha20Poly1305` (default) / `AesGcmSiv256` |
| `derive_cell_id(key_bytes) -> [u8; 32]` | BLAKE3 of the raw KV key, used as AAD |
| `encrypt_with_aead(&aead, &key, aad, pt)` | Seal, prepending a fresh random nonce → `nonce ‖ ct ‖ tag` |
| `decrypt_with_aead(&aead, &key, aad, wire)` | Inverse of `encrypt_with_aead` |

### Cell helpers (`cell` module)

| Item | Description |
|------|-------------|
| `CellId { table_id: u64, row_id: u64, col_id: u32 }` | Cell coordinate; `to_aad_bytes() -> [u8; 20]` (little-endian) |
| `encrypt_cell(&key_provider, cell_id, pt)` | Low-level seal → `nonce(24) ‖ ct ‖ tag(16)` |
| `decrypt_cell(&key_provider, cell_id, ct)` | Low-level open |
| `MIN_CIPHERTEXT_LEN` | Minimum cell-ciphertext length (40 = 24 + 16) |

### Key providers (`keys` module)

| Item | Description |
|------|-------------|
| `KeyProvider` trait | `Send + Sync` source of a 32-byte key: `get_key()`, `key32()` (validated cast) |
| `StaticKey` | In-memory key (`new(Vec<u8>)`, `from_array([u8; 32])`); `Debug` redacts material |
| `KeyringKey` | OS keyring provider — **stub** (M6 wiring pending); `get_key` returns `KeyringUnavailable`. Methods: `new(label)`, `label()` |

### Builder (`cipher_builder` module)

| Item | Description |
|------|-------------|
| `CipherBuilder` | Fluent builder: `new`, `aead(AeadChoice)`, `key([u8; 32])`, `passphrase(p, salt)`, `build(store)`, `build_test(store)` (fast Argon2id for tests) |
| `AeadChoice` | `XChaCha20Poly1305` (default) / `AesGcmSiv256` |
| `KeySource` | `Raw([u8; 32])` / `Passphrase { passphrase, salt }` (Argon2id-derived) |

### Keyring (`keyring` module)

| Item | Description |
|------|-------------|
| `Keyring` | Versioned KEK chain (last = active). Methods: `new(kek)`, `from_passphrase(pass, salt)`, `from_passphrase_with_params(pass, salt, params)`, `active_version`, `active_kek`, `kek_for_version`, `rotate(new_kek) -> u32`, `version_numbers` |
| `KeyVersion { version: u32, kek: [u8; 32] }` | A single versioned KEK; `Debug` redacts material |
| `generate_salt() -> [u8; 32]` | Random 32-byte salt for passphrase derivation |

### Transaction / snapshot wrappers

| Item | Description |
|------|-------------|
| `EncryptedTxn<'a>` | Transparent-encryption `KvTxn`; obtained from `EncryptedKv::transaction` |
| `EncryptedSnapshot<'a>` | Transparent-decryption `KvSnapshot`; obtained from `EncryptedKv::snapshot` |

### Re-exports

`oxistore_core::KvStore` is re-exported at the crate root for convenience.

## Algorithms

| Primitive | Details |
|-----------|---------|
| Data AEAD (default) | XChaCha20-Poly1305 — 192-bit nonce, 256-bit key, 128-bit tag |
| Data AEAD (alt) | AES-256-GCM-SIV — 96-bit nonce, 256-bit key, 128-bit tag, misuse-resistant |
| Cell-ID AAD | BLAKE3 of the raw KV key bytes (32 bytes) |
| DEK wrap AEAD | XChaCha20-Poly1305 |
| KDF | Argon2id (m = 65 536 KiB, t = 3, p = 1) via `oxicrypto` |
| RNG | OS CSPRNG via `oxicrypto::new_rng` |

### Wire formats

Cell-level: `nonce (24) ‖ ciphertext ‖ Poly1305 tag (16)`.

Envelope: `kek_version (4, LE) ‖ wrap_nonce (24) ‖ wrapped_dek (48) ‖ data_nonce (24) ‖ data_ciphertext (N + 16)` — minimum 116 bytes.

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `encrypt` | off | Marker feature (no extra deps); present for facade symmetry. `oxicrypto` is always a dependency |

## Error variants

`EncryptError` implements `std::error::Error` + `Display`, with `From` conversions to and from `oxistore_core::StoreError`.

| Variant | Description |
|---------|-------------|
| `InvalidKeyLength { got }` | Key is not exactly 32 bytes |
| `KeyringUnavailable { label }` | OS keyring entry unavailable (M6 stub) |
| `CiphertextTooShort { min_expected, got }` | Too few bytes to contain a nonce and/or tag |
| `AuthenticationFailed` | AEAD tag verification failed (tampered, wrong key/cell) |
| `RngFailed` | CSPRNG initialisation or fill failed |
| `Store(String)` | Underlying KV store error |
| `KeyDerivationFailed(String)` | Argon2id / PBKDF2 derivation failed |
| `MissingKekVersion(u32)` | Keyring has no entry for the requested KEK version |
| `EncryptionFailed(String)` | AEAD encryption failed during envelope sealing |
| `LockPoisoned` | Internal `RwLock` poisoned (a thread panicked while holding it) |
| `KeyringEmpty` | Keyring has no KEK versions (invariant violation) |
| `KeyRotation { old_version, new_version, reason }` | Key rotation failed |

## Cross-references

- [`oxistore`](https://crates.io/crates/oxistore) — the storage facade; enable the `encrypt` feature to re-export this crate.
- [`oxistore-core`](https://crates.io/crates/oxistore-core) — the `KvStore` / `KvTxn` / `KvSnapshot` / `StoreError` traits decorated by this crate.
- [`oxicrypto`](https://crates.io/crates/oxicrypto) — the Pure-Rust AEAD, BLAKE3, Argon2id, and CSPRNG primitives backing every operation.
- [`oxistore-kv-redb`](https://crates.io/crates/oxistore-kv-redb), [`oxistore-kv-sled`](https://crates.io/crates/oxistore-kv-sled), [`oxistore-kv-fjall`](https://crates.io/crates/oxistore-kv-fjall) — KV backends that can be wrapped.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxistore-encrypt` — cell-level and envelope AEAD encryption for OxiStore KV stores.
//!
//! This crate provides two encryption layers:
//!
//! ## Layer 1 — Cell-level encryption (`EncryptedKv`)
//!
//! - **[`CellId`]** — a `(table_id, row_id, col_id)` triple used as AAD in every
//!   AEAD operation, binding each ciphertext to its exact storage location.
//!
//! - **[`KeyProvider`]** — a fallible source of a 32-byte XChaCha20-Poly1305 key,
//!   with two built-in implementations:
//!   - [`StaticKey`] — in-memory `Vec<u8>` for tests and simple deployments.
//!   - [`KeyringKey`] — OS keyring stub (M6 wiring pending; returns an error now).
//!
//! - **[`encrypt_cell`] / [`decrypt_cell`]** — low-level AEAD helpers with the
//!   wire format `nonce (24 bytes) ‖ ciphertext ‖ Poly1305-tag (16 bytes)`.
//!
//! - **[`EncryptedKv<T, K, A>`]** — a [`KvStore`] decorator that encrypts all values
//!   on write and decrypts them transparently on read.  The third type parameter
//!   `A` selects the AEAD algorithm (defaults to XChaCha20-Poly1305).
//!
//! - **[`CipherBuilder`]** — fluent builder for constructing [`EncryptedKv`] with
//!   a selected cipher and key source.
//!
//! - **[`derive_cell_id`]** — BLAKE3-based cell ID derivation used as AAD.
//!
//! ## Layer 2 — Envelope encryption (`EncryptedKvEnvelope`)
//!
//! - **[`Keyring`]** — holds a versioned chain of Key-Encrypting Keys (KEKs).
//!   Supports passphrase derivation via Argon2id and cheap key rotation.
//!
//! - **[`EnvelopeCipher`]** — encrypts each value with a random Data Encryption
//!   Key (DEK), then wraps the DEK under the active KEK.  Key rotation re-wraps
//!   only the tiny DEK wrapper — bulk data is never re-encrypted.
//!
//! - **[`EncryptedKvEnvelope<T>`]** — a [`KvStore`] decorator using envelope
//!   encryption. Supports in-place key rotation via `rotate_kek`.
//!
//! - **[`rotate_all_keys`]** — free function to rotate all DEK wrappers in a
//!   raw `KvStore` to a new KEK.
//!
//! ## Envelope wire format
//!
//! ```text
//! ┌────────────┬──────────────┬─────────────┬───────────────┬───────────────────┐
//! │ kek_version│  wrap_nonce  │ wrapped_dek │  data_nonce   │  data_ciphertext  │
//! │  4 bytes   │  24 bytes    │  48 bytes   │  24 bytes     │  N+16 bytes       │
//! └────────────┴──────────────┴─────────────┴───────────────┴───────────────────┘
//! ```
//!
//! ## Algorithms
//!
//! | Primitive | Details |
//! |-----------|---------|
//! | Data AEAD | XChaCha20-Poly1305 (192-bit nonce, 256-bit key, 128-bit tag) |
//! | Data AEAD alt | AES-256-GCM-SIV (96-bit nonce, 256-bit key, 128-bit tag, misuse-resistant) |
//! | Cell ID AAD | BLAKE3 of raw KV key bytes (32 bytes) |
//! | DEK wrap AEAD | XChaCha20-Poly1305 (same) |
//! | KDF | Argon2id (m=65536 KiB, t=3, p=1) via `oxicrypto` |
//! | RNG | OS CSPRNG via `oxicrypto::new_rng` |
//!
//! ## Quick start
//!
//! ```no_run
//! use oxistore_encrypt::{EncryptedKv, StaticKey};
//! // Wrap any KvStore (here the type annotation is illustrative):
//! // let inner = redb_store; // any T: KvStore
//! // let key = StaticKey::from_array([0x42u8; 32]);
//! // let enc = EncryptedKv::new(inner, key);
//! // enc.put(b"hello", b"world").expect("put failed");
//! // assert_eq!(enc.get(b"hello").expect("get failed"), Some(b"world".to_vec()));
//! ```

pub mod aead;
pub mod cell;
pub mod cipher_builder;
pub mod decorator;
pub mod envelope;
pub mod error;
pub mod keyring;
pub mod keys;
pub mod snapshot;
pub mod txn;

pub use aead::{
    decrypt_with_aead, derive_cell_id, encrypt_with_aead, Aead, AeadKind, AesGcmSiv256Aead,
    XChaCha20Poly1305Aead,
};
pub use cell::{decrypt_cell, encrypt_cell, CellId, MIN_CIPHERTEXT_LEN};
pub use cipher_builder::{AeadChoice, CipherBuilder};
pub use decorator::EncryptedKv;
pub use envelope::{rotate_all_keys, EncryptedKvEnvelope, EnvelopeCipher, MIN_ENVELOPE_LEN};
pub use error::EncryptError;
pub use keyring::{generate_salt, KeyVersion, Keyring};
pub use keys::{KeyProvider, KeyringKey, StaticKey};
pub use snapshot::EncryptedSnapshot;
pub use txn::EncryptedTxn;
// Re-export the KvStore trait for convenience.
pub use oxistore_core::KvStore;

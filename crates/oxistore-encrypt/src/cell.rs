//! Cell-level AEAD encryption and decryption for `oxistore-encrypt`.
//!
//! # Wire format
//!
//! ```text
//! ┌─────────────┬──────────────────────────────────────────────────────────┐
//! │  nonce      │  ciphertext ‖ Poly1305-tag                               │
//! │  24 bytes   │  plaintext_len + 16 bytes                                │
//! └─────────────┴──────────────────────────────────────────────────────────┘
//! ```
//!
//! - **Nonce** — 24 random bytes generated freshly per encryption call.
//!   XChaCha20's 192-bit nonce space makes random nonces safe even at high
//!   volume (birthday-bound probability of collision is negligible for < 2^80
//!   messages under one key).
//!
//! - **AAD** — the [`CellId`] serialised as 20 bytes (little-endian):
//!   `[table_id: 8 bytes][row_id: 8 bytes][col_id: 4 bytes]`.  This binds
//!   each ciphertext to its exact storage location; moving or copying raw
//!   bytes to a different cell causes authentication to fail.
//!
//! - **Algorithm** — XChaCha20-Poly1305 (key: 32 bytes, nonce: 24 bytes,
//!   tag: 16 bytes) via `oxicrypto::XChaCha20Poly1305`.

use oxicrypto::{new_rng, XChaCha20Poly1305};

use crate::error::EncryptError;
use crate::keys::KeyProvider;

/// Minimum ciphertext length: nonce (24) + tag (16) + 0 bytes of plaintext.
pub const MIN_CIPHERTEXT_LEN: usize = NONCE_LEN + TAG_LEN;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

/// Identifies a single cell in a table: `(table_id, row_id, col_id)`.
///
/// The triple is serialised to 20 bytes of AAD on every encrypt/decrypt call,
/// binding the ciphertext to its precise storage location.
///
/// When the `serde` feature is enabled, `CellId` implements
/// [`serde::Serialize`] and [`serde::Deserialize`], allowing it to be stored
/// in JSON configuration files, persisted alongside encrypted metadata, or
/// transmitted over a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CellId {
    /// Table identifier.
    pub table_id: u64,
    /// Row identifier.
    pub row_id: u64,
    /// Column identifier.
    pub col_id: u32,
}

impl CellId {
    /// Serialise this [`CellId`] into 20 little-endian bytes used as AAD.
    pub fn to_aad_bytes(self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0..8].copy_from_slice(&self.table_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.row_id.to_le_bytes());
        buf[16..20].copy_from_slice(&self.col_id.to_le_bytes());
        buf
    }
}

/// Encrypt `plaintext` for the given `cell_id` using the key from `key_provider`.
///
/// A fresh 24-byte random nonce is generated on every call (via `oxicrypto`'s
/// OS-seeded CSPRNG).  The [`CellId`] is serialised and passed as AAD, binding
/// the ciphertext to its storage location.
///
/// # Output format
///
/// `nonce (24 bytes) ‖ ciphertext (plaintext.len() bytes) ‖ tag (16 bytes)`
///
/// # Errors
///
/// * [`EncryptError::InvalidKeyLength`] — key is not 32 bytes.
/// * [`EncryptError::KeyringUnavailable`] — provider cannot supply a key.
/// * [`EncryptError::RngFailed`] — OS RNG unavailable.
pub fn encrypt_cell<K: KeyProvider>(
    key_provider: &K,
    cell_id: CellId,
    plaintext: &[u8],
) -> Result<Vec<u8>, EncryptError> {
    let key32 = key_provider.key32()?;

    // Generate a fresh random nonce.
    let mut rng = new_rng().map_err(|_| EncryptError::RngFailed)?;
    let mut nonce = [0u8; NONCE_LEN];
    rng.fill(&mut nonce).map_err(|_| EncryptError::RngFailed)?;

    let aad = cell_id.to_aad_bytes();

    // Allocate output: nonce || ciphertext || tag.
    let ct_len = plaintext
        .len()
        .checked_add(TAG_LEN)
        .ok_or(EncryptError::RngFailed)?; // virtually impossible overflow
    let mut output = vec![0u8; NONCE_LEN + ct_len];
    output[..NONCE_LEN].copy_from_slice(&nonce);

    let cipher = XChaCha20Poly1305;
    let written = cipher
        .seal(key32, &nonce, &aad, plaintext, &mut output[NONCE_LEN..])
        .map_err(|_| EncryptError::AuthenticationFailed)?;

    // Truncate to actual length (should equal nonce_len + ct_len).
    output.truncate(NONCE_LEN + written);
    Ok(output)
}

/// Decrypt a cell-level ciphertext produced by [`encrypt_cell`].
///
/// The first 24 bytes of `ciphertext_with_nonce` are parsed as the nonce;
/// the remainder is authenticated ciphertext with an appended Poly1305 tag.
///
/// The same `cell_id` used during encryption must be supplied; mismatching IDs
/// cause authentication failure.
///
/// # Errors
///
/// * [`EncryptError::CiphertextTooShort`] — fewer than `NONCE_LEN + TAG_LEN` bytes.
/// * [`EncryptError::InvalidKeyLength`] — key is not 32 bytes.
/// * [`EncryptError::KeyringUnavailable`] — provider cannot supply a key.
/// * [`EncryptError::AuthenticationFailed`] — tag mismatch (tampered or wrong cell).
pub fn decrypt_cell<K: KeyProvider>(
    key_provider: &K,
    cell_id: CellId,
    ciphertext_with_nonce: &[u8],
) -> Result<Vec<u8>, EncryptError> {
    if ciphertext_with_nonce.len() < MIN_CIPHERTEXT_LEN {
        return Err(EncryptError::CiphertextTooShort {
            min_expected: MIN_CIPHERTEXT_LEN,
            got: ciphertext_with_nonce.len(),
        });
    }

    let key32 = key_provider.key32()?;

    let nonce: &[u8; NONCE_LEN] = ciphertext_with_nonce[..NONCE_LEN].try_into().map_err(|_| {
        EncryptError::CiphertextTooShort {
            min_expected: MIN_CIPHERTEXT_LEN,
            got: ciphertext_with_nonce.len(),
        }
    })?;

    let ct = &ciphertext_with_nonce[NONCE_LEN..];
    let aad = cell_id.to_aad_bytes();

    let pt_len = ct.len().saturating_sub(TAG_LEN);
    let mut plaintext = vec![0u8; pt_len];

    let cipher = XChaCha20Poly1305;
    cipher
        .open(key32, nonce, &aad, ct, &mut plaintext)
        .map_err(|_| EncryptError::AuthenticationFailed)?;

    Ok(plaintext)
}

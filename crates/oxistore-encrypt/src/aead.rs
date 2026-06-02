//! AEAD cipher abstraction for `oxistore-encrypt`.
//!
//! Defines the [`Aead`] trait and two built-in implementations:
//!
//! - [`XChaCha20Poly1305Aead`] — 24-byte nonce, 16-byte tag.
//! - [`AesGcmSiv256Aead`] — 12-byte nonce, 16-byte tag, misuse-resistant.
//!
//! Also provides [`derive_cell_id`], a BLAKE3-based deterministic key binding
//! used as AAD in every AEAD operation.

use oxicrypto::{blake3, new_rng, AesGcmSiv256, XChaCha20Poly1305};

use crate::error::EncryptError;

// ── Cell ID derivation ────────────────────────────────────────────────────────

/// Derive a stable 32-byte cell identifier from `key_bytes` using BLAKE3.
///
/// The result is used as AAD (additional authenticated data) in every AEAD
/// seal/open call.  Because the same KV key always produces the same cell ID,
/// ciphertexts are bound to their storage location.  Moving or copying a
/// ciphertext to a different key causes authentication to fail ("transplant
/// attack" prevention).
pub fn derive_cell_id(key_bytes: &[u8]) -> [u8; 32] {
    blake3(key_bytes)
}

// ── Aead trait ────────────────────────────────────────────────────────────────

/// AEAD cipher abstraction used by [`EncryptedKv`](crate::EncryptedKv).
///
/// All implementations must be `Send + Sync` so that they can be shared
/// across threads (e.g. stored in `Arc<EncryptedKv<…>>`).
///
/// # Wire format produced by `seal`
///
/// The [`Aead`] implementations in this crate do **not** prepend a nonce to
/// the output; nonce management is done by the caller.  `seal` returns only
/// `ciphertext ‖ tag` (the nonce is allocated and prepended by the higher-
/// level encrypt helper in the `crypto` module).
pub trait Aead: Send + Sync {
    /// Length of the nonce required by this cipher (in bytes).
    fn nonce_len(&self) -> usize;

    /// Length of the authentication tag appended to ciphertext (in bytes).
    fn tag_len(&self) -> usize;

    /// Encrypt `pt` with `key`, `nonce`, and `aad`.
    ///
    /// Returns `ciphertext ‖ tag` (no nonce prepended).
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::AuthenticationFailed`] on AEAD failure, or
    /// [`EncryptError::RngFailed`] on buffer allocation issues.
    fn seal(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        pt: &[u8],
    ) -> Result<Vec<u8>, EncryptError>;

    /// Decrypt `ct` (ciphertext ‖ tag) with `key`, `nonce`, and `aad`.
    ///
    /// Returns plaintext on success.
    ///
    /// # Errors
    ///
    /// Returns [`EncryptError::AuthenticationFailed`] if the tag does not
    /// verify (wrong key, tampered data, or wrong nonce/AAD).
    fn open(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        ct: &[u8],
    ) -> Result<Vec<u8>, EncryptError>;
}

// ── XChaCha20-Poly1305 ────────────────────────────────────────────────────────

/// [`Aead`] implementation backed by XChaCha20-Poly1305.
///
/// - Nonce: 24 bytes (192-bit; random nonces are safe at high volume)
/// - Tag: 16 bytes
/// - Key: 32 bytes
#[derive(Debug, Clone, Copy, Default)]
pub struct XChaCha20Poly1305Aead;

impl Aead for XChaCha20Poly1305Aead {
    fn nonce_len(&self) -> usize {
        24
    }
    fn tag_len(&self) -> usize {
        16
    }

    fn seal(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        pt: &[u8],
    ) -> Result<Vec<u8>, EncryptError> {
        let tag_len = self.tag_len();
        let out_len = pt
            .len()
            .checked_add(tag_len)
            .ok_or(EncryptError::RngFailed)?;
        let mut out = vec![0u8; out_len];
        let nonce24: &[u8; 24] =
            nonce
                .try_into()
                .map_err(|_| EncryptError::CiphertextTooShort {
                    min_expected: 24,
                    got: nonce.len(),
                })?;
        let cipher = XChaCha20Poly1305;
        cipher
            .seal(key, nonce24, aad, pt, &mut out)
            .map_err(|_| EncryptError::AuthenticationFailed)?;
        Ok(out)
    }

    fn open(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        ct: &[u8],
    ) -> Result<Vec<u8>, EncryptError> {
        let tag_len = self.tag_len();
        if ct.len() < tag_len {
            return Err(EncryptError::CiphertextTooShort {
                min_expected: tag_len,
                got: ct.len(),
            });
        }
        let pt_len = ct.len() - tag_len;
        let mut pt = vec![0u8; pt_len];
        let nonce24: &[u8; 24] =
            nonce
                .try_into()
                .map_err(|_| EncryptError::CiphertextTooShort {
                    min_expected: 24,
                    got: nonce.len(),
                })?;
        let cipher = XChaCha20Poly1305;
        cipher
            .open(key, nonce24, aad, ct, &mut pt)
            .map_err(|_| EncryptError::AuthenticationFailed)?;
        Ok(pt)
    }
}

// ── AES-256-GCM-SIV ──────────────────────────────────────────────────────────

/// [`Aead`] implementation backed by AES-256-GCM-SIV (RFC 8452).
///
/// AES-256-GCM-SIV is misuse-resistant: nonce reuse does not expose the
/// plaintext (only reveals whether the same message was encrypted twice).
///
/// - Nonce: 12 bytes (96-bit)
/// - Tag: 16 bytes
/// - Key: 32 bytes
#[derive(Debug, Clone, Copy, Default)]
pub struct AesGcmSiv256Aead;

impl Aead for AesGcmSiv256Aead {
    fn nonce_len(&self) -> usize {
        12
    }
    fn tag_len(&self) -> usize {
        16
    }

    fn seal(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        pt: &[u8],
    ) -> Result<Vec<u8>, EncryptError> {
        let tag_len = self.tag_len();
        let out_len = pt
            .len()
            .checked_add(tag_len)
            .ok_or(EncryptError::RngFailed)?;
        let mut out = vec![0u8; out_len];
        let nonce12: &[u8; 12] =
            nonce
                .try_into()
                .map_err(|_| EncryptError::CiphertextTooShort {
                    min_expected: 12,
                    got: nonce.len(),
                })?;
        let cipher = AesGcmSiv256;
        cipher
            .seal(key, nonce12, aad, pt, &mut out)
            .map_err(|_| EncryptError::AuthenticationFailed)?;
        Ok(out)
    }

    fn open(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        ct: &[u8],
    ) -> Result<Vec<u8>, EncryptError> {
        let tag_len = self.tag_len();
        if ct.len() < tag_len {
            return Err(EncryptError::CiphertextTooShort {
                min_expected: tag_len,
                got: ct.len(),
            });
        }
        let pt_len = ct.len() - tag_len;
        let mut pt = vec![0u8; pt_len];
        let nonce12: &[u8; 12] =
            nonce
                .try_into()
                .map_err(|_| EncryptError::CiphertextTooShort {
                    min_expected: 12,
                    got: nonce.len(),
                })?;
        let cipher = AesGcmSiv256;
        cipher
            .open(key, nonce12, aad, ct, &mut pt)
            .map_err(|_| EncryptError::AuthenticationFailed)?;
        Ok(pt)
    }
}

// ── Enum dispatch variant ─────────────────────────────────────────────────────

/// Statically-dispatched enum combining both built-in AEAD choices.
///
/// Using an enum avoids heap allocation (`Box<dyn Aead>`) while still
/// allowing the choice to be made at runtime (e.g. by [`crate::CipherBuilder`]).
#[derive(Debug, Clone, Copy, Default)]
pub enum AeadKind {
    /// XChaCha20-Poly1305 (24-byte nonce). Default.
    #[default]
    XChaCha20Poly1305,
    /// AES-256-GCM-SIV (12-byte nonce, misuse-resistant).
    AesGcmSiv256,
}

impl Aead for AeadKind {
    fn nonce_len(&self) -> usize {
        match self {
            AeadKind::XChaCha20Poly1305 => XChaCha20Poly1305Aead.nonce_len(),
            AeadKind::AesGcmSiv256 => AesGcmSiv256Aead.nonce_len(),
        }
    }

    fn tag_len(&self) -> usize {
        match self {
            AeadKind::XChaCha20Poly1305 => XChaCha20Poly1305Aead.tag_len(),
            AeadKind::AesGcmSiv256 => AesGcmSiv256Aead.tag_len(),
        }
    }

    fn seal(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        pt: &[u8],
    ) -> Result<Vec<u8>, EncryptError> {
        match self {
            AeadKind::XChaCha20Poly1305 => XChaCha20Poly1305Aead.seal(key, nonce, aad, pt),
            AeadKind::AesGcmSiv256 => AesGcmSiv256Aead.seal(key, nonce, aad, pt),
        }
    }

    fn open(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        ct: &[u8],
    ) -> Result<Vec<u8>, EncryptError> {
        match self {
            AeadKind::XChaCha20Poly1305 => XChaCha20Poly1305Aead.open(key, nonce, aad, ct),
            AeadKind::AesGcmSiv256 => AesGcmSiv256Aead.open(key, nonce, aad, ct),
        }
    }
}

// ── Shared encrypt/decrypt helpers ────────────────────────────────────────────

/// Encrypt `pt` with `aead` and `key`, prepending a fresh random nonce.
///
/// Wire format: `nonce (nonce_len bytes) ‖ ciphertext ‖ tag (tag_len bytes)`.
pub fn encrypt_with_aead<A: Aead>(
    aead: &A,
    key: &[u8; 32],
    aad: &[u8],
    pt: &[u8],
) -> Result<Vec<u8>, EncryptError> {
    let nonce_len = aead.nonce_len();
    let mut nonce = vec![0u8; nonce_len];
    let mut rng = new_rng().map_err(|_| EncryptError::RngFailed)?;
    rng.fill(&mut nonce).map_err(|_| EncryptError::RngFailed)?;

    let ct_with_tag = aead.seal(key, &nonce, aad, pt)?;

    let mut out = Vec::with_capacity(nonce_len + ct_with_tag.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct_with_tag);
    Ok(out)
}

/// Decrypt ciphertext produced by [`encrypt_with_aead`].
///
/// The first `nonce_len` bytes are the nonce; the remainder is `ciphertext ‖ tag`.
pub fn decrypt_with_aead<A: Aead>(
    aead: &A,
    key: &[u8; 32],
    aad: &[u8],
    wire: &[u8],
) -> Result<Vec<u8>, EncryptError> {
    let nonce_len = aead.nonce_len();
    let min_len = nonce_len + aead.tag_len();
    if wire.len() < min_len {
        return Err(EncryptError::CiphertextTooShort {
            min_expected: min_len,
            got: wire.len(),
        });
    }
    let nonce = &wire[..nonce_len];
    let ct_with_tag = &wire[nonce_len..];
    aead.open(key, nonce, aad, ct_with_tag)
}

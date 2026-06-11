//! aws-lc-rs backed AEAD bridge for oxistore-encrypt cell-level encryption.
//!
//! [`AwsLcOxistoreAead`] wraps [`oxicrypto_adapter_aws_lc::aead::AwsLcAead`] and
//! implements the [`crate::aead::Aead`] trait, so `EncryptedKv::with_aead` can use
//! FIPS-validated aws-lc-rs AEADs (AES-256-GCM-SIV/GCM, ChaCha20-Poly1305) as the
//! cell-level cipher instead of the default XChaCha20-Poly1305 path.
//!
//! Requires the `oxicrypto-aws-lc` feature.

use oxicrypto::Aead as CryptoAead;
use oxicrypto_adapter_aws_lc::aead::AwsLcAead;

use crate::aead::Aead as StoreAead;
use crate::EncryptError;

/// Bridge adapter exposing aws-lc-rs AEADs to oxistore-encrypt cell encryption.
#[derive(Debug, Clone, Copy)]
pub struct AwsLcOxistoreAead {
    inner: AwsLcAead,
}

impl AwsLcOxistoreAead {
    /// AES-256-GCM-SIV via aws-lc-rs (12-byte nonce, 32-byte key, misuse-resistant).
    pub fn aes256_gcm_siv() -> Self {
        Self {
            inner: AwsLcAead::aes256_gcm_siv(),
        }
    }

    /// AES-256-GCM via aws-lc-rs (12-byte nonce, 32-byte key).
    pub fn aes256_gcm() -> Self {
        Self {
            inner: AwsLcAead::aes256_gcm(),
        }
    }

    /// ChaCha20-Poly1305 via aws-lc-rs (12-byte nonce, 32-byte key).
    pub fn chacha20_poly1305() -> Self {
        Self {
            inner: AwsLcAead::chacha20_poly1305(),
        }
    }
}

impl StoreAead for AwsLcOxistoreAead {
    fn nonce_len(&self) -> usize {
        CryptoAead::nonce_len(&self.inner)
    }

    fn tag_len(&self) -> usize {
        CryptoAead::tag_len(&self.inner)
    }

    fn seal(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        pt: &[u8],
    ) -> Result<Vec<u8>, EncryptError> {
        let tag_len = CryptoAead::tag_len(&self.inner);
        let out_len = pt
            .len()
            .checked_add(tag_len)
            .ok_or(EncryptError::RngFailed)?;
        let mut out = vec![0u8; out_len];
        CryptoAead::seal(&self.inner, key.as_slice(), nonce, aad, pt, &mut out)
            .map_err(|e| EncryptError::EncryptionFailed(e.to_string()))?;
        Ok(out)
    }

    fn open(
        &self,
        key: &[u8; 32],
        nonce: &[u8],
        aad: &[u8],
        ct: &[u8],
    ) -> Result<Vec<u8>, EncryptError> {
        let tag_len = CryptoAead::tag_len(&self.inner);
        if ct.len() < tag_len {
            return Err(EncryptError::CiphertextTooShort {
                min_expected: tag_len,
                got: ct.len(),
            });
        }
        let pt_len = ct.len() - tag_len;
        let mut pt = vec![0u8; pt_len];
        CryptoAead::open(&self.inner, key.as_slice(), nonce, aad, ct, &mut pt)
            .map_err(|_| EncryptError::AuthenticationFailed)?;
        Ok(pt)
    }
}

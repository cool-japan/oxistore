//! Tamper-detection tests for `oxistore-encrypt`.
//!
//! These tests verify that any modification to the ciphertext — whether in the
//! ciphertext body, the authentication tag, or the nonce — is detected and
//! rejected during decryption.

use oxistore_encrypt::{decrypt_cell, encrypt_cell, CellId, EncryptError, StaticKey};

fn test_key() -> StaticKey {
    StaticKey::from_array([0xBBu8; 32])
}

fn default_cell() -> CellId {
    CellId {
        table_id: 42,
        row_id: 1337,
        col_id: 7,
    }
}

fn encrypt_plaintext(plaintext: &[u8]) -> Vec<u8> {
    encrypt_cell(&test_key(), default_cell(), plaintext).expect("encrypt failed")
}

// ── Ciphertext body corruption ────────────────────────────────────────────────

#[test]
fn flip_byte_in_ciphertext_body_rejected() {
    let plaintext = b"this must not be readable";
    let mut ct = encrypt_plaintext(plaintext);
    // Ciphertext body starts at byte 24 (after nonce) and ends before the last 16 tag bytes.
    let body_start = 24;
    let body_end = ct.len() - 16;

    // Only flip if there is a ciphertext body (non-empty plaintext).
    if body_start < body_end {
        ct[body_start] ^= 0xFF;
    } else {
        // Empty-body case: flip the first tag byte instead.
        ct[body_start] ^= 0x01;
    }

    let result = decrypt_cell(&test_key(), default_cell(), &ct);
    assert!(
        matches!(result, Err(EncryptError::AuthenticationFailed)),
        "corrupted ciphertext body must trigger auth failure, got: {result:?}"
    );
}

#[test]
fn flip_byte_in_tag_rejected() {
    let plaintext = b"tamper the tag";
    let mut ct = encrypt_plaintext(plaintext);
    // Tag is the last 16 bytes.
    let tag_offset = ct.len() - 16;
    ct[tag_offset] ^= 0xAA;

    let result = decrypt_cell(&test_key(), default_cell(), &ct);
    assert!(
        matches!(result, Err(EncryptError::AuthenticationFailed)),
        "corrupted tag must trigger auth failure, got: {result:?}"
    );
}

#[test]
fn flip_last_tag_byte_rejected() {
    let plaintext = b"another tamper test";
    let mut ct = encrypt_plaintext(plaintext);
    let last = ct.len() - 1;
    ct[last] ^= 0x01;

    let result = decrypt_cell(&test_key(), default_cell(), &ct);
    assert!(
        matches!(result, Err(EncryptError::AuthenticationFailed)),
        "last tag byte corruption must trigger auth failure, got: {result:?}"
    );
}

// ── Nonce region corruption ───────────────────────────────────────────────────

#[test]
fn flip_byte_in_nonce_region_rejected() {
    // Flipping a nonce byte changes the decryption context — the resulting
    // plaintext (if any) will be garbage and the tag will not verify.
    let plaintext = b"nonce must be authentic";
    let mut ct = encrypt_plaintext(plaintext);
    ct[3] ^= 0x55; // byte 3 is inside the nonce region [0..24)

    let result = decrypt_cell(&test_key(), default_cell(), &ct);
    assert!(
        matches!(result, Err(EncryptError::AuthenticationFailed)),
        "corrupted nonce must trigger auth failure, got: {result:?}"
    );
}

// ── Truncation ────────────────────────────────────────────────────────────────

#[test]
fn truncated_to_zero_bytes_rejected() {
    let ct: &[u8] = &[];
    let result = decrypt_cell(&test_key(), default_cell(), ct);
    assert!(
        matches!(result, Err(EncryptError::CiphertextTooShort { .. })),
        "empty input must return CiphertextTooShort, got: {result:?}"
    );
}

#[test]
fn truncated_to_23_bytes_rejected() {
    // Needs at least 24 (nonce) + 16 (tag) = 40 bytes.
    let ct = vec![0u8; 23];
    let result = decrypt_cell(&test_key(), default_cell(), &ct);
    assert!(
        matches!(result, Err(EncryptError::CiphertextTooShort { .. })),
        "23-byte input must return CiphertextTooShort, got: {result:?}"
    );
}

#[test]
fn truncated_to_39_bytes_rejected() {
    let ct = vec![0u8; 39];
    let result = decrypt_cell(&test_key(), default_cell(), &ct);
    assert!(
        matches!(result, Err(EncryptError::CiphertextTooShort { .. })),
        "39-byte input must return CiphertextTooShort, got: {result:?}"
    );
}

// ── Wrong key ─────────────────────────────────────────────────────────────────

#[test]
fn wrong_key_rejected() {
    let good_key = StaticKey::from_array([0xBBu8; 32]);
    let bad_key = StaticKey::from_array([0xCCu8; 32]);
    let cell_id = default_cell();

    let ct = encrypt_cell(&good_key, cell_id, b"secret data").expect("encrypt failed");
    let result = decrypt_cell(&bad_key, cell_id, &ct);
    assert!(
        matches!(result, Err(EncryptError::AuthenticationFailed)),
        "wrong key must trigger auth failure, got: {result:?}"
    );
}

// ── Invalid key length ────────────────────────────────────────────────────────

#[test]
fn short_key_rejected_at_encrypt() {
    let bad_key = StaticKey::new(vec![0xFFu8; 16]); // 16 bytes, not 32
    let result = encrypt_cell(&bad_key, default_cell(), b"data");
    assert!(
        matches!(result, Err(EncryptError::InvalidKeyLength { .. })),
        "16-byte key must return InvalidKeyLength at encrypt, got: {result:?}"
    );
}

#[test]
fn short_key_rejected_at_decrypt() {
    let good_key = test_key();
    let ct = encrypt_cell(&good_key, default_cell(), b"data").expect("encrypt failed");

    let bad_key = StaticKey::new(vec![0xFFu8; 16]); // 16 bytes
    let result = decrypt_cell(&bad_key, default_cell(), &ct);
    assert!(
        matches!(result, Err(EncryptError::InvalidKeyLength { .. })),
        "16-byte key must return InvalidKeyLength at decrypt, got: {result:?}"
    );
}

// ── Empty plaintext edge case ─────────────────────────────────────────────────

#[test]
fn empty_plaintext_tamper_rejected() {
    // For empty plaintext the output is exactly 40 bytes (24 nonce + 0 ct + 16 tag).
    let ct = encrypt_plaintext(b"");
    assert_eq!(ct.len(), 40, "empty plaintext should yield 40-byte output");

    let mut corrupted = ct.clone();
    corrupted[30] ^= 0x01; // flip a tag byte

    let result = decrypt_cell(&test_key(), default_cell(), &corrupted);
    assert!(
        matches!(result, Err(EncryptError::AuthenticationFailed)),
        "tampered empty-plaintext ciphertext must fail, got: {result:?}"
    );
}

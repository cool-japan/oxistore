//! Tests that verify truncated or corrupted compressed input returns a proper
//! [`CompressError::Decompress`] error rather than panicking.
//!
//! These tests only run when the `compress` feature is enabled.

#![cfg(feature = "compress")]

use oxistore_compress::{CompressError, OxiArcCodec};

/// Truncating the compressed output by 16 bytes must return a decompression
/// error rather than succeeding or panicking.
#[test]
fn truncated_input_returns_decompress_error() {
    let codec = OxiArcCodec::new();
    let data = b"hello world, this is test data for truncation testing - longer to ensure meaningful DEFLATE output";
    let mut compressed = codec.compress(data).expect("compress");

    // Truncate: remove the last 16 bytes (or as many as available, leaving ≥1).
    let truncate_len = compressed.len().saturating_sub(16).max(1);
    compressed.truncate(truncate_len);

    let result = codec.decompress(&compressed);
    assert!(
        result.is_err(),
        "expected Err on truncated input, got Ok({:?})",
        result.ok()
    );

    let err = result.expect_err("already checked is_err above");
    // Must be a Decompress variant.
    assert!(
        matches!(err, CompressError::Decompress(_)),
        "expected CompressError::Decompress, got {err:?}"
    );
    // Error message must be non-empty.
    assert!(
        !err.to_string().is_empty(),
        "error message should not be empty"
    );
}

/// Completely empty input must return a decompression error (not panic, not Ok).
#[test]
fn empty_compressed_input_returns_decompress_error() {
    let codec = OxiArcCodec::new();
    let result = codec.decompress(&[]);
    assert!(
        result.is_err(),
        "expected Err on empty compressed input, got Ok({:?})",
        result.ok()
    );
    let err = result.expect_err("already checked is_err above");
    assert!(
        matches!(err, CompressError::Decompress(_)),
        "expected CompressError::Decompress, got {err:?}"
    );
}

/// Single-byte corrupt stream must return a decompression error.
#[test]
fn single_byte_corrupt_returns_decompress_error() {
    let codec = OxiArcCodec::new();
    // 0xFF is not a valid DEFLATE stream on its own.
    let result = codec.decompress(&[0xFF]);
    assert!(
        result.is_err(),
        "expected Err on single-byte corrupt input, got Ok({:?})",
        result.ok()
    );
}

/// Half-truncation: truncate at the 50% point of a compressed buffer.
#[test]
fn half_truncated_input_returns_decompress_error() {
    let codec = OxiArcCodec::new();
    let data: Vec<u8> = (0u8..=255).cycle().take(512).collect();
    let compressed = codec.compress(&data).expect("compress");
    let half = compressed.len() / 2;
    let truncated = &compressed[..half.max(1)];

    let result = codec.decompress(truncated);
    assert!(
        result.is_err(),
        "expected Err on half-truncated input, got Ok({:?})",
        result.ok()
    );
}

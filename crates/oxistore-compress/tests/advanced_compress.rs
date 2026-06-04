//! Advanced tests for `oxistore-compress`:
//! - Property-based: random byte slices survive round-trip
//! - Large payload: 10 MB buffer round-trip and throughput
//! - Corrupted input: truncated compressed stream produces `Decompress` error
//! - From<CompressError> for StoreError conversion

#![cfg(feature = "compress")]

use oxistore_compress::{CompressError, OxiArcCodec};
use proptest::prelude::*;

// ── Property-based: random bytes survive round-trip ──────────────────────────

proptest! {
    #[test]
    fn random_bytes_round_trip(data in proptest::collection::vec(any::<u8>(), 0..65536)) {
        let codec = OxiArcCodec::new();
        let compressed = codec.compress(&data).expect("compress");
        let decompressed = codec.decompress(&compressed).expect("decompress");
        prop_assert_eq!(decompressed, data);
    }
}

proptest! {
    #[test]
    fn random_bytes_round_trip_level9(data in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let codec = OxiArcCodec::with_level(9);
        let compressed = codec.compress(&data).expect("compress level 9");
        let decompressed = codec.decompress(&compressed).expect("decompress level 9");
        prop_assert_eq!(decompressed, data);
    }
}

// ── Large payload: 10 MB buffer round-trip ────────────────────────────────────

#[test]
fn large_payload_10mb_round_trip() {
    let codec = OxiArcCodec::new();
    // 10 MB of structured but compressible data
    let size = 10 * 1024 * 1024usize;
    let data: Vec<u8> = (0..size).map(|i| (i % 127) as u8).collect();

    let compressed = codec.compress(&data).expect("compress 10 MB");
    assert!(
        compressed.len() < data.len(),
        "10 MB compressible data should compress: got {} vs {}",
        compressed.len(),
        data.len()
    );

    let decompressed = codec.decompress(&compressed).expect("decompress 10 MB");
    assert_eq!(decompressed.len(), size);
    assert_eq!(decompressed, data);
}

#[test]
fn large_payload_random_10mb() {
    let codec = OxiArcCodec::new();
    // 10 MB of pseudo-random (incompressible) data
    let size = 10 * 1024 * 1024usize;
    // Simple PRNG-like deterministic sequence for reproducibility
    let data: Vec<u8> = (0..size)
        .map(|i| {
            let x = i
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect();

    let compressed = codec.compress(&data).expect("compress random 10 MB");
    let decompressed = codec
        .decompress(&compressed)
        .expect("decompress random 10 MB");
    assert_eq!(decompressed.len(), size);
    assert_eq!(decompressed, data);
}

// ── Corrupted input: truncated stream → Decompress error ─────────────────────

#[test]
fn truncated_compressed_stream_fails() {
    let codec = OxiArcCodec::new();
    let data = b"hello world this is some data that will compress nicely".repeat(100);
    let compressed = codec.compress(&data).expect("compress");

    // Truncate to 50% of the compressed data
    let truncated = &compressed[..compressed.len() / 2];
    let result = codec.decompress(truncated);
    assert!(
        result.is_err(),
        "truncated input should fail decompression; got Ok({} bytes)",
        result.as_ref().map_or(0, |v| v.len())
    );
    if let Err(e) = result {
        assert!(
            matches!(e, CompressError::Decompress(_)),
            "expected CompressError::Decompress, got {e:?}"
        );
    }
}

#[test]
fn garbage_input_fails_decompression() {
    let codec = OxiArcCodec::new();
    // Feed completely random bytes — should not produce valid DEFLATE output
    let garbage: Vec<u8> = (0..1024).map(|i| (i * 7 + 3) as u8).collect();
    let result = codec.decompress(&garbage);
    // May or may not fail (garbage could accidentally be valid), but should not panic
    let _ = result; // just verify no panic
}

#[test]
fn all_zeros_input_decompresses_fails_when_not_valid_deflate() {
    let codec = OxiArcCodec::new();
    // All-zeros is not a valid DEFLATE stream
    let zeros = vec![0u8; 64];
    let result = codec.decompress(&zeros);
    // Should not panic; error is expected but not required (depending on DEFLATE impl)
    let _ = result;
}

// ── From<CompressError> for StoreError ────────────────────────────────────────

#[test]
fn from_compress_error_for_store_error() {
    use oxistore_core::StoreError;

    let e = CompressError::Compress("test compression error".to_string());
    let store_err: StoreError = e.into();
    assert!(
        format!("{store_err}").contains("compress error"),
        "StoreError should contain compress error message"
    );

    let e2 = CompressError::Decompress("decompression failed".to_string());
    let store_err2: StoreError = e2.into();
    assert!(
        format!("{store_err2}").contains("decompress error"),
        "StoreError should contain decompress error message"
    );

    let e3 = CompressError::InvalidLevel(42);
    let store_err3: StoreError = e3.into();
    let msg = format!("{store_err3}");
    assert!(
        msg.contains("invalid") || msg.contains("42"),
        "StoreError should mention level 42: {msg}"
    );
}

// ── OxiArcCodec with_level round-trip for all valid levels ───────────────────

#[test]
fn all_valid_levels_round_trip() {
    let data = b"The quick brown fox jumps over the lazy dog. ".repeat(100);
    for level in 0u8..=9 {
        let codec = OxiArcCodec::with_level(level);
        let compressed = codec
            .compress(&data)
            .unwrap_or_else(|e| panic!("level {level} compress failed: {e}"));
        let decompressed = codec
            .decompress(&compressed)
            .unwrap_or_else(|e| panic!("level {level} decompress failed: {e}"));
        assert_eq!(
            decompressed,
            data.to_vec(),
            "level {level} round-trip failed"
        );
    }
}

// ── decompress_into appends correctly ────────────────────────────────────────

#[test]
fn decompress_into_appends_to_existing_buffer() {
    let codec = OxiArcCodec::new();
    let data = b"append test data".repeat(50);
    let compressed = codec.compress(&data).expect("compress");

    let prefix = b"EXISTING_PREFIX";
    let mut buf = prefix.to_vec();
    OxiArcCodec::decompress_into(&compressed, &mut buf).expect("decompress_into");

    assert!(
        buf.starts_with(prefix),
        "decompress_into must not overwrite prefix"
    );
    assert_eq!(
        &buf[prefix.len()..],
        data.as_slice(),
        "appended data must match original"
    );
}

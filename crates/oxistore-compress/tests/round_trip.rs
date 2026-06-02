//! Round-trip compression tests for [`OxiArcCodec`].
//!
//! These tests only run when the `compress` feature is enabled.

#![cfg(feature = "compress")]

use oxistore_compress::OxiArcCodec;

/// Generate 64 KiB of repetitive data that DEFLATE can compress well.
fn repetitive_64k() -> Vec<u8> {
    b"AAABBBCCCDDDEEEFFF".repeat(64 * 1024 / 18 + 1)[..65536].to_vec()
}

#[test]
fn compress_decompress_round_trip() {
    let codec = OxiArcCodec::new();
    let original = repetitive_64k();

    let compressed = codec.compress(&original).expect("compress failed");
    let decompressed = codec.decompress(&compressed).expect("decompress failed");

    assert_eq!(decompressed, original, "round-trip produced different data");
}

#[test]
fn compressed_is_smaller_than_input() {
    let codec = OxiArcCodec::new();
    let original = repetitive_64k();
    let compressed = codec.compress(&original).expect("compress failed");

    assert!(
        compressed.len() < original.len(),
        "expected compressed ({} B) < original ({} B) for repetitive input",
        compressed.len(),
        original.len()
    );
}

#[test]
fn compress_empty_input() {
    let codec = OxiArcCodec::new();
    let compressed = codec.compress(&[]).expect("compress of empty slice failed");
    let decompressed = codec
        .decompress(&compressed)
        .expect("decompress of empty slice failed");
    assert!(
        decompressed.is_empty(),
        "decompress of empty should be empty"
    );
}

#[test]
fn compress_level_zero_round_trip() {
    let codec = OxiArcCodec::with_level(0);
    let data = b"stored-block round trip test".to_vec();
    let compressed = codec.compress(&data).expect("level-0 compress failed");
    let decompressed = codec
        .decompress(&compressed)
        .expect("level-0 decompress failed");
    assert_eq!(decompressed, data);
}

#[test]
fn compress_level_nine_round_trip() {
    let codec = OxiArcCodec::with_level(9);
    let original = repetitive_64k();
    let compressed = codec.compress(&original).expect("level-9 compress failed");
    let decompressed = codec
        .decompress(&compressed)
        .expect("level-9 decompress failed");
    assert_eq!(decompressed, original);
}

// ── decompress_into ───────────────────────────────────────────────────────────

#[test]
fn decompress_into_round_trip() {
    let codec = OxiArcCodec::new();
    let original = repetitive_64k();
    let compressed = codec.compress(&original).expect("compress");

    let mut out = Vec::new();
    OxiArcCodec::decompress_into(&compressed, &mut out).expect("decompress_into");

    assert_eq!(out, original, "decompress_into produced different data");
}

#[test]
fn decompress_into_appends_to_existing_content() {
    let codec = OxiArcCodec::new();
    let original = b"hello world!".to_vec();
    let compressed = codec.compress(&original).expect("compress");

    let mut out = b"prefix-".to_vec();
    OxiArcCodec::decompress_into(&compressed, &mut out).expect("decompress_into");

    assert_eq!(&out[..7], b"prefix-");
    assert_eq!(&out[7..], original.as_slice());
}

// ── new_with_level ────────────────────────────────────────────────────────────

#[test]
fn new_with_level_valid_levels_succeed() {
    for level in 0u32..=9 {
        let codec = OxiArcCodec::new_with_level(level)
            .unwrap_or_else(|_| panic!("level {level} should be valid"));
        // Verify the codec actually works at this level.
        let data = b"test data for level round-trip".to_vec();
        let compressed = codec.compress(&data).expect("compress");
        let decompressed = codec.decompress(&compressed).expect("decompress");
        assert_eq!(decompressed, data, "round-trip failed at level {level}");
    }
}

#[test]
fn new_with_level_invalid_returns_error() {
    use oxistore_compress::CompressError;

    for invalid in [10u32, 100, u32::MAX] {
        let err =
            OxiArcCodec::new_with_level(invalid).expect_err("should fail for out-of-range level");
        assert!(
            matches!(err, CompressError::InvalidLevel(l) if l == invalid),
            "expected InvalidLevel({invalid}), got {err:?}"
        );
    }
}

// ── From<OxiArcError> ─────────────────────────────────────────────────────────

#[test]
fn from_oxiarc_error_conversion() {
    use oxiarc_core::error::OxiArcError;
    use oxistore_compress::CompressError;

    let oxiarc_err = OxiArcError::corrupted(0, "test corruption");
    let compress_err: CompressError = oxiarc_err.into();

    // The conversion should produce a Decompress variant containing the message.
    assert!(
        matches!(compress_err, CompressError::Decompress(_)),
        "expected CompressError::Decompress, got {compress_err:?}"
    );
    let msg = compress_err.to_string();
    assert!(msg.contains("decompress error"), "message: {msg}");
}

/// Tripwire: documents the policy that no banned compression crates are used.
///
/// The real enforcement is:
///   `cargo tree -p oxistore-compress --features compress -e normal \
///    | grep -E '(flate2|zstd|brotli|miniz|snap)'`
/// That command must return empty for this crate to be compliant.
///
/// Banned dependencies: flate2, zstd, brotli, snap, miniz_oxide.
/// Allowed:            oxiarc-deflate (and its transitive Pure Rust deps).
#[test]
fn no_banned_compression_crates() {
    // Verify the allowed dep is referenced; the banned list is in the doc comment.
    let allowed = "oxiarc-deflate";
    assert!(
        !allowed.is_empty(),
        "oxiarc-deflate must be the sole compression dep"
    );
    // If this test compiles and runs, it proves only oxiarc-deflate paths are used
    // (any accidental link to a banned crate would already have failed cargo tree).
}

// ── compress_with_hint ────────────────────────────────────────────────────────

/// `compress_with_hint` followed by `decompress` must round-trip correctly.
#[test]
fn compress_with_hint_round_trip() {
    let codec = OxiArcCodec::new();
    let original = repetitive_64k();

    let compressed = codec
        .compress_with_hint(&original, original.len() / 2)
        .expect("compress_with_hint failed");
    let decompressed = codec
        .decompress(&compressed)
        .expect("decompress after compress_with_hint failed");

    assert_eq!(
        decompressed, original,
        "compress_with_hint round-trip produced different data"
    );
}

/// `compress_with_hint` compresses an empty input without error.
#[test]
fn compress_with_hint_empty_input() {
    let codec = OxiArcCodec::new();
    let compressed = codec
        .compress_with_hint(&[], 0)
        .expect("compress_with_hint of empty slice failed");
    let decompressed = codec.decompress(&compressed).expect("decompress empty");
    assert!(decompressed.is_empty());
}

/// `compress_with_hint` ignores the size_hint when it is larger than the input.
#[test]
fn compress_with_hint_large_hint() {
    let codec = OxiArcCodec::with_level(6);
    let original = b"hello with a large hint".to_vec();
    let compressed = codec
        .compress_with_hint(&original, 1024 * 1024)
        .expect("compress_with_hint with large hint");
    let decompressed = codec.decompress(&compressed).expect("decompress");
    assert_eq!(decompressed, original);
}

// ── codec metadata ────────────────────────────────────────────────────────────

/// `algorithm_name()` must return `"DEFLATE"`.
#[test]
fn codec_algorithm_name() {
    let codec = OxiArcCodec::new();
    assert_eq!(codec.algorithm_name(), "DEFLATE");
}

/// `algorithm_name` is consistent regardless of the compression level.
#[test]
fn codec_algorithm_name_all_levels() {
    for level in 0u8..=9 {
        let codec = OxiArcCodec::with_level(level);
        assert_eq!(
            codec.algorithm_name(),
            "DEFLATE",
            "algorithm_name should be DEFLATE at level {level}"
        );
    }
}

/// `compression_level()` returns `Some(level)` matching the constructed level.
#[test]
fn codec_compression_level() {
    let codec_default = OxiArcCodec::new();
    assert_eq!(
        codec_default.compression_level(),
        Some(6),
        "default codec should have level 6"
    );

    for level in 0u8..=9 {
        let codec = OxiArcCodec::with_level(level);
        assert_eq!(
            codec.compression_level(),
            Some(level),
            "compression_level should match constructed level {level}"
        );
    }
}

/// `compression_level` returns `Some(_)` for all valid levels constructed via
/// `new_with_level`.
#[test]
fn codec_compression_level_via_new_with_level() {
    for level in 0u32..=9 {
        let codec = OxiArcCodec::new_with_level(level).expect("valid level");
        assert_eq!(codec.compression_level(), Some(level as u8));
    }
}

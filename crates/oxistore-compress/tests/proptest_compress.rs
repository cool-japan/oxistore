//! Property-based and regression tests for the oxistore-compress crate.
//!
//! Uses `proptest` to verify that the DEFLATE codec provided by
//! [`OxiArcCodec`] is a correct, lossless, deterministic round-trip.
//!
//! The `compress` feature must be enabled for these tests.

#[cfg(feature = "compress")]
mod compress_tests {
    use oxistore_compress::OxiArcCodec;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(200))]

        /// Any byte vector survives a compress → decompress round-trip unchanged.
        #[test]
        fn prop_compress_round_trip(
            data in proptest::collection::vec(any::<u8>(), 0..4096),
        ) {
            let codec = OxiArcCodec::new();
            let input_len = data.len();
            let compressed = codec.compress(&data)
                .expect("compress must not fail");
            let decompressed = codec.decompress(&compressed)
                .expect("decompress must not fail");
            prop_assert_eq!(
                decompressed, data,
                "round-trip mismatch for input of length {}",
                input_len
            );
        }
    }

    /// Compressing and decompressing an empty slice yields an empty slice.
    #[test]
    fn prop_compress_empty() {
        let codec = OxiArcCodec::new();
        let compressed = codec.compress(b"").expect("compress empty must succeed");
        let decompressed = codec
            .decompress(&compressed)
            .expect("decompress empty must succeed");
        assert_eq!(
            decompressed, b"",
            "round-trip of empty input must yield empty output"
        );
    }

    /// Every single-byte input round-trips correctly.
    #[test]
    fn prop_compress_single_byte() {
        let codec = OxiArcCodec::new();
        for b in 0u8..=255 {
            let input = [b];
            let compressed = codec
                .compress(&input)
                .unwrap_or_else(|e| panic!("compress([{b}]) failed: {e}"));
            let decompressed = codec
                .decompress(&compressed)
                .unwrap_or_else(|e| panic!("decompress([{b}]) failed: {e}"));
            assert_eq!(
                decompressed.as_slice(),
                &input[..],
                "round-trip mismatch for byte={b}"
            );
        }
    }

    /// The same input always produces the same compressed output (determinism).
    #[test]
    fn prop_compress_deterministic() {
        let codec = OxiArcCodec::new();

        let inputs: &[&[u8]] = &[
            b"",
            b"hello",
            b"the quick brown fox jumps over the lazy dog",
            &[0u8; 1024],
            &[0xABu8; 512],
        ];

        for input in inputs {
            let c1 = codec
                .compress(input)
                .unwrap_or_else(|e| panic!("compress failed: {e}"));
            let c2 = codec
                .compress(input)
                .unwrap_or_else(|e| panic!("compress failed on repeat: {e}"));
            assert_eq!(
                c1,
                c2,
                "compress is not deterministic for input of length {}",
                input.len()
            );
        }
    }
}

// When the `compress` feature is absent there is nothing to test; prevent a
// "no tests" warning by providing a trivial always-pass test.
#[cfg(not(feature = "compress"))]
#[test]
fn compress_feature_disabled() {
    // Tests are gated on the `compress` feature; nothing to run here.
}

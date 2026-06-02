//! Tests for the Parquet [`Codec`](parquet::compression::Codec) shim.
//!
//! These tests only run when the `compress` feature is enabled.

#![cfg(feature = "compress")]

use oxistore_compress::OxiArcCodec;
use parquet::compression::Codec as ParquetCodec;

/// Sample buffer used across tests — a small repetitive block.
fn sample() -> Vec<u8> {
    b"parquet shim round-trip sample data ".repeat(200)
}

#[test]
fn parquet_codec_compress_decompress() {
    let mut codec = OxiArcCodec::new();
    let input = sample();

    // Compress via the Parquet Codec trait.
    let mut compressed = Vec::new();
    ParquetCodec::compress(&mut codec, &input, &mut compressed).expect("parquet compress failed");

    // Decompress via the Parquet Codec trait — supply the size hint.
    let mut output = Vec::new();
    let written = ParquetCodec::decompress(&mut codec, &compressed, &mut output, Some(input.len()))
        .expect("parquet decompress failed");

    assert_eq!(written, input.len(), "written bytes mismatch");
    assert_eq!(output, input, "round-trip produced different data");
}

#[test]
fn parquet_codec_compress_decompress_no_hint() {
    let mut codec = OxiArcCodec::new();
    let input = sample();

    let mut compressed = Vec::new();
    ParquetCodec::compress(&mut codec, &input, &mut compressed).expect("parquet compress failed");

    // No uncompress_size hint — codec must still work.
    let mut output = Vec::new();
    let written = ParquetCodec::decompress(&mut codec, &compressed, &mut output, None)
        .expect("parquet decompress with no hint failed");

    assert_eq!(written, input.len());
    assert_eq!(output, input);
}

#[test]
fn parquet_codec_appends_to_existing_buffer() {
    let mut codec = OxiArcCodec::new();
    let input = b"small payload".to_vec();

    let mut compressed = Vec::new();
    ParquetCodec::compress(&mut codec, &input, &mut compressed).expect("compress failed");

    // Start with non-empty output_buf — codec must append, not overwrite.
    let prefix: Vec<u8> = vec![0xFF, 0xFE, 0xFD];
    let mut output = prefix.clone();
    let written = ParquetCodec::decompress(&mut codec, &compressed, &mut output, None)
        .expect("decompress failed");

    assert_eq!(
        &output[..prefix.len()],
        prefix.as_slice(),
        "prefix was overwritten"
    );
    assert_eq!(
        &output[prefix.len()..],
        input.as_slice(),
        "appended data mismatch"
    );
    assert_eq!(written, input.len());
}

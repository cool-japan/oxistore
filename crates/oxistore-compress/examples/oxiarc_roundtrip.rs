//! `OxiArcCodec` round trip: Pure Rust DEFLATE (via `oxiarc-deflate`), never
//! `flate2`/`zstd`/`brotli`/`miniz_oxide`.
//!
//! Requires the `compress` feature (off by default, since it pulls in the
//! `parquet` dependency for the [`parquet::compression::Codec`] bridge):
//!
//! ```sh
//! cargo run -p oxistore-compress --example oxiarc_roundtrip --features compress
//! ```

use oxistore_compress::OxiArcCodec;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compressible payload: long English text with lots of repetition.
    let payload = "the quick brown fox jumps over the lazy dog. ".repeat(200);
    let payload = payload.as_bytes();

    // ── Default codec (level 6, balanced) ─────────────────────────────────
    let codec = OxiArcCodec::new();
    let compressed = codec.compress(payload)?;
    let decompressed = codec.decompress(&compressed)?;
    assert_eq!(decompressed, payload);
    println!(
        "level 6 (default): {} bytes -> {} bytes ({:.1}% of original), algorithm={}",
        payload.len(),
        compressed.len(),
        100.0 * compressed.len() as f64 / payload.len() as f64,
        codec.algorithm_name()
    );

    // ── Compression level sweep: 0 (store) vs 9 (best) ────────────────────
    for level in [0u8, 1, 6, 9] {
        let codec = OxiArcCodec::with_level(level);
        let compressed = codec.compress(payload)?;
        println!(
            "level {level}: {} bytes ({:?})",
            compressed.len(),
            codec.compression_level()
        );
    }

    // `new_with_level` validates the range (0..=9) and returns an error
    // instead of silently clamping.
    match OxiArcCodec::new_with_level(10) {
        Err(e) => println!("new_with_level(10) correctly rejected: {e}"),
        Ok(_) => println!("new_with_level(10) unexpectedly accepted"),
    }

    // ── compress_with_hint / decompress_into: avoid an extra allocation ──
    let codec = OxiArcCodec::with_level(6);
    let compressed = codec.compress_with_hint(payload, payload.len())?;
    let mut out = Vec::with_capacity(payload.len());
    OxiArcCodec::decompress_into(&compressed, &mut out)?;
    assert_eq!(out, payload);
    println!(
        "compress_with_hint + decompress_into round-trip -> {} bytes, verified",
        out.len()
    );

    // ── Round trip on incompressible (random-looking) data still works ───
    // DEFLATE has a small worst-case expansion on data it can't compress;
    // this codec doesn't special-case that, it just round-trips correctly.
    let mut pseudo_random = Vec::with_capacity(4096);
    let mut state: u64 = 0x2545F4914F6CDD1D;
    for _ in 0..4096 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        pseudo_random.push((state & 0xFF) as u8);
    }
    let codec = OxiArcCodec::new();
    let compressed = codec.compress(&pseudo_random)?;
    let decompressed = codec.decompress(&compressed)?;
    assert_eq!(decompressed, pseudo_random);
    println!(
        "incompressible 4096-byte input -> {} bytes compressed (expansion is expected and fine)",
        compressed.len()
    );

    Ok(())
}

//! Criterion benchmarks for `oxistore-compress` operations.
//!
//! Covers compress + decompress roundtrip throughput via [`OxiArcCodec`]
//! at three payload sizes: 1 KiB, 64 KiB, and 1 MiB.
//!
//! Requires the `compress` feature: `cargo bench -p oxistore-compress --features compress`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use oxistore_compress::OxiArcCodec;

// ── compress roundtrip ────────────────────────────────────────────────────────

fn bench_compress_roundtrip(c: &mut Criterion) {
    let codec = OxiArcCodec::new();
    let mut group = c.benchmark_group("compress_roundtrip");

    for size in [1_024usize, 65_536, 1_048_576] {
        // Use a repetitive pattern so compression has something to do.
        let data: Vec<u8> = (0u8..=255).cycle().take(size).collect();
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, input| {
            b.iter(|| {
                let compressed = codec.compress(input).expect("compress");
                let _decompressed = codec.decompress(&compressed).expect("decompress");
            });
        });
    }

    group.finish();
}

// ── compress-only ─────────────────────────────────────────────────────────────

fn bench_compress_only(c: &mut Criterion) {
    let codec = OxiArcCodec::new();
    let mut group = c.benchmark_group("compress_only");

    for size in [1_024usize, 65_536, 1_048_576] {
        let data: Vec<u8> = (0u8..=255).cycle().take(size).collect();
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, input| {
            b.iter(|| codec.compress(input).expect("compress"));
        });
    }

    group.finish();
}

// ── decompress-only ───────────────────────────────────────────────────────────

fn bench_decompress_only(c: &mut Criterion) {
    let codec = OxiArcCodec::new();
    let mut group = c.benchmark_group("decompress_only");

    for size in [1_024usize, 65_536, 1_048_576] {
        let data: Vec<u8> = (0u8..=255).cycle().take(size).collect();
        // Pre-compress so we only measure decompression.
        let compressed = codec.compress(&data).expect("pre-compress");
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &compressed,
            |b, input| {
                b.iter(|| codec.decompress(input).expect("decompress"));
            },
        );
    }

    group.finish();
}

// ── codec-shim overhead ───────────────────────────────────────────────────────
//
// Compares the overhead of calling `OxiArcCodec::compress` / `decompress`
// directly (inherent method) versus going through the Parquet
// [`parquet::compression::Codec`] trait shim.  The difference quantifies the
// vtable + buffer-append bookkeeping cost of the shim layer.

fn bench_codec_shim_overhead(c: &mut Criterion) {
    use parquet::compression::Codec as ParquetCodec;

    let mut group = c.benchmark_group("codec_shim_overhead");
    // Use a fixed 64 KiB repetitive payload — same as other bench groups.
    let data: Vec<u8> = (0u8..=255).cycle().take(65_536).collect();
    group.throughput(Throughput::Bytes(data.len() as u64));

    // ── inherent API ──────────────────────────────────────────────────────────
    group.bench_function("inherent_compress", |b| {
        let codec = OxiArcCodec::new();
        b.iter(|| codec.compress(&data).expect("inherent compress"));
    });

    // ── Parquet Codec shim ────────────────────────────────────────────────────
    group.bench_function("shim_compress", |b| {
        let mut codec = OxiArcCodec::new();
        let mut out = Vec::with_capacity(data.len());
        b.iter(|| {
            out.clear();
            ParquetCodec::compress(&mut codec, &data, &mut out).expect("shim compress");
        });
    });

    // Pre-compress so decompress benchmarks measure decompression only.
    let inherent_codec = OxiArcCodec::new();
    let compressed = inherent_codec.compress(&data).expect("pre-compress");

    // ── inherent decompress ───────────────────────────────────────────────────
    group.bench_function("inherent_decompress", |b| {
        let codec = OxiArcCodec::new();
        b.iter(|| codec.decompress(&compressed).expect("inherent decompress"));
    });

    // ── Parquet Codec shim decompress ─────────────────────────────────────────
    group.bench_function("shim_decompress", |b| {
        let mut codec = OxiArcCodec::new();
        let mut out = Vec::with_capacity(data.len());
        b.iter(|| {
            out.clear();
            ParquetCodec::decompress(&mut codec, &compressed, &mut out, Some(data.len()))
                .expect("shim decompress");
        });
    });

    group.finish();
}

// ── criterion entry points ────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_compress_roundtrip,
    bench_compress_only,
    bench_decompress_only,
    bench_codec_shim_overhead,
);
criterion_main!(benches);

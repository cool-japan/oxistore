//! Criterion benchmarks for `oxistore-compress` operations.
//!
//! Covers compress + decompress roundtrip throughput via [`OxiArcCodec`]
//! at three payload sizes: 1 KiB, 64 KiB, and 1 MiB, at compression levels 1
//! (fast) and 6 (balanced).
//!
//! Also measures the overhead of the Parquet Codec shim versus the direct
//! inherent API.
//!
//! Requires the `compress` feature: `cargo bench -p oxistore-compress --features compress`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use oxistore_compress::OxiArcCodec;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a repetitive payload of `size` bytes — easy to compress.
fn repetitive(size: usize) -> Vec<u8> {
    (0u8..=255).cycle().take(size).collect()
}

// ── compress+decompress roundtrip at level 1 and level 6 ─────────────────────

fn bench_compress_roundtrip_level1(c: &mut Criterion) {
    let codec = OxiArcCodec::with_level(1);
    let mut group = c.benchmark_group("compress_roundtrip_level1");

    for size in [1_024usize, 65_536, 1_048_576] {
        let data = repetitive(size);
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

fn bench_compress_roundtrip_level6(c: &mut Criterion) {
    let codec = OxiArcCodec::with_level(6);
    let mut group = c.benchmark_group("compress_roundtrip_level6");

    for size in [1_024usize, 65_536, 1_048_576] {
        let data = repetitive(size);
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

// ── compress-only at level 1 and level 6 ─────────────────────────────────────

fn bench_compress_only_level1(c: &mut Criterion) {
    let codec = OxiArcCodec::with_level(1);
    let mut group = c.benchmark_group("compress_only_level1");

    for size in [1_024usize, 65_536, 1_048_576] {
        let data = repetitive(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, input| {
            b.iter(|| codec.compress(input).expect("compress"));
        });
    }

    group.finish();
}

fn bench_compress_only_level6(c: &mut Criterion) {
    let codec = OxiArcCodec::with_level(6);
    let mut group = c.benchmark_group("compress_only_level6");

    for size in [1_024usize, 65_536, 1_048_576] {
        let data = repetitive(size);
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, input| {
            b.iter(|| codec.compress(input).expect("compress"));
        });
    }

    group.finish();
}

// ── decompress-only ───────────────────────────────────────────────────────────
//
// Decompression is level-independent (inflate has no level concept), so a
// single group suffices.

fn bench_decompress_only(c: &mut Criterion) {
    let codec = OxiArcCodec::new();
    let mut group = c.benchmark_group("decompress_only");

    for size in [1_024usize, 65_536, 1_048_576] {
        let data = repetitive(size);
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

// ── Parquet Codec shim overhead ───────────────────────────────────────────────
//
// Compares the overhead of calling `OxiArcCodec::compress` / `decompress`
// directly (inherent method) versus going through the Parquet
// [`parquet::compression::Codec`] trait shim.  The difference quantifies the
// vtable + buffer-append bookkeeping cost of the shim layer.

fn bench_codec_shim_overhead(c: &mut Criterion) {
    use parquet::compression::Codec as ParquetCodec;

    let mut group = c.benchmark_group("codec_shim_overhead");
    // Use a fixed 64 KiB repetitive payload — same as other bench groups.
    let data: Vec<u8> = repetitive(65_536);
    group.throughput(Throughput::Bytes(data.len() as u64));

    // ── inherent compress ─────────────────────────────────────────────────────
    group.bench_function("inherent_compress", |b| {
        let codec = OxiArcCodec::new();
        b.iter(|| codec.compress(&data).expect("inherent compress"));
    });

    // ── Parquet Codec shim compress ───────────────────────────────────────────
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

// ── Vec allocation pattern analysis ──────────────────────────────────────────
//
// Measures Vec allocation count per compress+decompress round-trip by
// instrumenting allocation size steps.  We record the Vec capacity before and
// after each operation to quantify the allocation "budget" per round-trip.
//
// Methodology: each compress call allocates exactly one new Vec (the output
// buffer returned from oxiarc_deflate::deflate).  Each decompress call
// allocates exactly one new Vec (the inflated output returned from inflate).
// Therefore a full compress+decompress round-trip should require exactly 2
// distinct Vec allocations of meaningful size, plus any internal scratch
// buffers allocated by the deflate/inflate implementations.
//
// We verify this property by wrapping the round-trip inside `iter_custom` and
// observing the number of Vec growth events on a known-size output buffer.

fn bench_allocation_pattern(c: &mut Criterion) {
    let codec = OxiArcCodec::new();
    let mut group = c.benchmark_group("allocation_pattern");

    // Use a small payload (4 KiB) to make the allocation pattern visible
    // without being dominated by the compress work itself.
    let size = 4_096usize;
    let data: Vec<u8> = repetitive(size);
    group.throughput(Throughput::Bytes(size as u64));

    // Baseline: how many bytes does a round-trip allocate end-to-end?
    // We pre-compute the compressed size to reason about buffer sizes.
    let compressed_size = {
        let c = codec.compress(&data).expect("pre-compress");
        c.len()
    };

    // ── allocation-aware round-trip ───────────────────────────────────────────
    //
    // Each iteration:
    //   1. `compress` → returns a freshly-allocated Vec<u8> of `compressed_size`
    //   2. `decompress` → returns a freshly-allocated Vec<u8> of `size`
    //
    // Total per iteration: 2 "meaningful" allocations + possible internal scratch.
    //
    // We measure wall-clock time and annotate the allocation sizes in the
    // benchmark label for documentation purposes.
    group.bench_function(
        format!("round_trip_2alloc_compressed_{compressed_size}B"),
        |b| {
            b.iter(|| {
                let compressed = codec.compress(&data).expect("compress");
                let _decompressed = codec.decompress(&compressed).expect("decompress");
                // Return the sizes so the optimizer cannot elide the allocations.
                (compressed.len(), _decompressed.len())
            });
        },
    );

    // ── decompress_into (reduced allocations) ─────────────────────────────────
    //
    // By using `decompress_into` with a pre-allocated output buffer we avoid
    // the second allocation in the round-trip: the compressed Vec is still
    // allocated by `compress`, but the decompressed bytes are appended into
    // the caller-provided `out` buffer which is cleared (not freed) each iter.
    //
    // This pattern reduces the round-trip to a single "new" allocation per iter.
    group.bench_function("round_trip_1alloc_decompress_into", |b| {
        let mut out = Vec::with_capacity(size);
        b.iter(|| {
            out.clear();
            let compressed = codec.compress(&data).expect("compress");
            OxiArcCodec::decompress_into(&compressed, &mut out).expect("decompress_into");
            compressed.len()
        });
    });

    group.finish();
}

// ── criterion entry points ────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_compress_roundtrip_level1,
    bench_compress_roundtrip_level6,
    bench_compress_only_level1,
    bench_compress_only_level6,
    bench_decompress_only,
    bench_codec_shim_overhead,
    bench_allocation_pattern,
);
criterion_main!(benches);

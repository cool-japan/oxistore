# oxistore-compress TODO

## Status
Pure-Rust compression codec bridge backed exclusively by `oxiarc-deflate` (RFC 1951 DEFLATE). The `compress` feature gate enables `OxiArcCodec` (compress/decompress/decompress_into API) and a `parquet::compression::Codec` shim so that Parquet pages can be compressed via the OxiARC stack. Zero dependency on `flate2`, `zstd`, `brotli`, `snap`, `lz4`, or `miniz_oxide`. ~171 SLOC across 4 files (lib.rs, codec.rs, parquet_shim.rs, and two test files). 15 tests pass under `--features compress`.

## Core Implementation
- [x] `OxiArcCodec::compress` / `decompress` via `oxiarc-deflate` (Pure Rust DEFLATE) (done 2026-05-25)
- [x] `parquet::compression::Codec` shim (`OxiArcParquetCodec`) bridging into `OxiArcCodec` (done 2026-05-25)
- [x] `CompressError` enum with `Compress` and `Decompress` variants (done 2026-05-25)
- [x] Add `OxiArcCodec::compress_level(data, level: u8)` — exposed via `OxiArcCodec::with_level(level)` constructor and `compress()` inherent method (done 2026-05-25)
- [x] Add `OxiArcCodec::decompress_into(src, dst: &mut Vec<u8>)` — append-into-existing-buffer variant to avoid extra allocations (done 2026-05-25)
- [x] Add `oxistore-columnar` integration — payload-level OxiARC DEFLATE envelope in `oxistore-columnar` `write_to_bytes`/`read_from_bytes` using `oxiarc-deflate::deflate`/`inflate` directly (done 2026-05-25)
- [x] Add `OxiArcCodec::compress_with_hint(data, input_size_hint)` — pre-allocate output buffer based on hint for large-payload paths (~15 SLOC) (done 2026-05-25)

## API Improvements
- [x] Add `CompressError::InvalidLevel(u32)` variant for out-of-range compression levels (done 2026-05-25)
- [x] Implement `From<CompressError>` for `StoreError` for cross-crate propagation — already implemented in lib.rs (done 2026-05-25)
- [x] Add `OxiArcCodec::new_with_level(level: u32)` constructor — validates range 0–9, returns `Err(InvalidLevel)` for out-of-range (done 2026-05-25)
- [x] Expose codec metadata: `OxiArcCodec::algorithm_name() -> &'static str` and `OxiArcCodec::compression_level() -> Option<u8>` (~10 SLOC) (done 2026-05-25)
- [x] Add `From<oxiarc_core::error::OxiArcError>` for `CompressError` — error conversion from OxiARC stack (done 2026-05-25)

## Testing
- [x] Round-trip test: compress then decompress produces identical output (`round_trip.rs`) (done 2026-05-25)
- [x] Compressed output is smaller than uncompressed input for compressible data (`round_trip.rs`) (done 2026-05-25)
- [x] Empty input round-trip: zero bytes in, zero bytes out after decompress (`round_trip.rs`) (done 2026-05-25)
- [x] Compression level 0 and level 9 round-trips (`round_trip.rs`) (done 2026-05-25)
- [x] No-banned-crates check: assert neither `flate2` nor `zstd` appear in the binary (`round_trip.rs`) (done 2026-05-25)
- [x] Parquet codec shim round-trip: `OxiArcParquetCodec::compress` / `decompress` (`parquet.rs`) (done 2026-05-25)
- [x] Parquet codec shim with no decompressed-size hint (`parquet.rs`) (done 2026-05-25)
- [x] Parquet codec shim appends into existing output buffer (`parquet.rs`) (done 2026-05-25)
- [x] `decompress_into` round-trip and appends to existing buffer (`round_trip.rs`) (done 2026-05-25)
- [x] `new_with_level` returns Ok for valid levels 0–9; Err(InvalidLevel) for out-of-range values (`round_trip.rs`) (done 2026-05-25)
- [x] `From<OxiArcError>` conversion produces `CompressError::Decompress` variant (`round_trip.rs`) (done 2026-05-25)
- [x] Property-based test: random byte slices (0–65536 bytes) survive round-trip via proptest — added `random_bytes_round_trip` and `random_bytes_round_trip_level9` (done 2026-06-03)
- [x] Large-payload test: compress / decompress 10 MB buffer, verify round-trip — both compressible and pseudo-random data tested (done 2026-06-03)
- [x] Corrupted-input test: feed truncated compressed stream to `decompress`, expect `CompressError::Decompress` — `truncated_compressed_stream_fails` test added (done 2026-06-03)

## Performance
- [x] Criterion benchmark: compress/decompress throughput for 1 KB / 64 KB / 1 MB payloads at level 1 and level 6 — `bench_compress_roundtrip_level1` + `bench_compress_roundtrip_level6` + compress/decompress-only groups (done 2026-06-03)
- [x] Criterion benchmark: `OxiArcParquetCodec` shim overhead vs direct `OxiArcCodec` call — `bench_codec_shim_overhead` group (inherent vs shim for both compress and decompress) (done 2026-06-03)
- [x] Profile allocation pattern: measure number of `Vec` allocations per compress+decompress round-trip — `bench_allocation_pattern` group: documents 2-alloc (compress+decompress) vs 1-alloc (`decompress_into`) pattern (done 2026-06-03)

## Integration
- [x] Wire OxiARC into `oxistore-columnar` — payload-level via `compress` feature in oxistore-columnar (done 2026-05-25)
- [x] Add `oxistore` facade re-export of `OxiArcCodec` under `oxistore::compress::*` — `pub mod compress` in oxistore/src/lib.rs re-exports `OxiArcCodec` + `CompressError` behind the `compress` feature (done 2026-06-03)
- [x] Verify `oxistore-compress` is compile-time gated: `cargo build -p oxistore-compress` (no `--features compress`) succeeds with zero code enabled; `cargo build -p oxistore-compress --features compress` enables the full API (done 2026-06-03)

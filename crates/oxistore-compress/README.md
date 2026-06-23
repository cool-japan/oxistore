# oxistore-compress — Pure-Rust OxiARC compression bridge for OxiStore

[![Crates.io](https://img.shields.io/crates/v/oxistore-compress.svg)](https://crates.io/crates/oxistore-compress)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxistore-compress` is the compression layer of the OxiStore stack. It provides a single DEFLATE codec — [`OxiArcCodec`] — backed **exclusively** by `oxiarc-deflate` (Pure-Rust DEFLATE, RFC 1951) from the COOLJAPAN OxiARC stack, plus a [`parquet::compression::Codec`] shim so that Parquet pages can be compressed and decompressed through the same Pure-Rust path.

In line with the COOLJAPAN Pure-Rust compression policy, this crate **never** depends on `flate2`, `zstd`, `brotli`, `snap`, or `miniz_oxide`. The only codec backend is OxiARC. The `CompressError` error type integrates with the wider stack via a `From<CompressError>` conversion into `oxistore_core::StoreError`.

## Installation

```toml
[dependencies]
# Codec + Parquet shim require the `compress` feature.
oxistore-compress = { version = "0.2.0", features = ["compress"] }
```

Without the `compress` feature only the `CompressError` type is compiled; the codec and Parquet shim are feature-gated.

## Quick Start

```rust,no_run
# #[cfg(feature = "compress")]
# {
use oxistore_compress::OxiArcCodec;

let codec = OxiArcCodec::new();                 // level 6 (balanced)
let data = b"hello, world! ".repeat(1000);

let compressed = codec.compress(&data)?;
let decompressed = codec.decompress(&compressed)?;
assert_eq!(decompressed, data);
# }
# Ok::<(), oxistore_compress::CompressError>(())
```

### Choosing a level

```rust,no_run
# #[cfg(feature = "compress")]
# {
use oxistore_compress::OxiArcCodec;

let fast = OxiArcCodec::with_level(1);          // clamps values > 9 to 9
let best = OxiArcCodec::new_with_level(9)?;     // errors on level > 9
assert_eq!(best.algorithm_name(), "DEFLATE");
assert_eq!(best.compression_level(), Some(9));
# }
# Ok::<(), oxistore_compress::CompressError>(())
```

### Parquet page compression

With the `compress` feature, `OxiArcCodec` implements `parquet::compression::Codec`, so it can be slotted directly into a Parquet writer/reader pipeline to compress pages with OxiARC DEFLATE instead of any C-backed codec.

## API Overview

### `OxiArcCodec` (feature `compress`)

A stateless, `Copy` DEFLATE codec. Carries only a compression level (0 = store, 9 = best).

| Method | Description |
|--------|-------------|
| `OxiArcCodec::new()` | `const` constructor at the default level (6, balanced) |
| `OxiArcCodec::with_level(level: u8)` | `const` constructor; values above 9 are clamped to 9 |
| `OxiArcCodec::new_with_level(level: u32)` | Fallible constructor; returns `CompressError::InvalidLevel` for `level > 9` |
| `compress(&self, data)` | Compress with DEFLATE → `Vec<u8>` |
| `decompress(&self, data)` | Decompress a DEFLATE stream → `Vec<u8>` |
| `OxiArcCodec::decompress_into(data, &mut out)` | Decompress, appending to an existing buffer (associated fn) |
| `compress_with_hint(&self, data, size_hint)` | Compress with an advisory size hint (currently delegates to `compress`) |
| `algorithm_name(&self)` | `const` — always `"DEFLATE"` |
| `compression_level(&self)` | `const` — `Some(level)`, level in 0–9 |

### `parquet::compression::Codec` impl (feature `compress`)

`OxiArcCodec` implements the Parquet `Codec` trait (`compress` / `decompress`) via the `parquet_shim` module, bridging Parquet page compression to OxiARC DEFLATE.

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `compress` | off | Enables `OxiArcCodec` and the Parquet `Codec` shim. Pulls in `oxiarc-deflate`, `oxiarc-core`, and `parquet`. **Never** pulls in flate2, zstd, brotli, snap, or miniz_oxide |

## Error variants

`CompressError` is `#[non_exhaustive]` and implements `std::error::Error` + `Display`. It converts into `oxistore_core::StoreError` (via `StoreError::Other`), and (under the `compress` feature) `From<oxiarc_core::error::OxiArcError>` maps OxiARC errors into it.

| Variant | Description |
|---------|-------------|
| `Compress(String)` | Compression failure (wraps the underlying error message) |
| `Decompress(String)` | Decompression failure (wraps the underlying error message) |
| `InvalidLevel(u32)` | Requested compression level is outside the valid range 0–9 |

## OxiARC compression backend

The sole backend is **`oxiarc-deflate`** — a Pure-Rust RFC 1951 DEFLATE implementation from the COOLJAPAN OxiARC stack. `compress` calls `oxiarc_deflate::deflate(data, level)`; `decompress` calls `oxiarc_deflate::inflate(data)`. No C, C++, or Fortran code is linked at any point.

## Cross-references

- [`oxistore`](https://crates.io/crates/oxistore) — the storage facade; enable the `compress` feature to re-export this crate (it forwards `oxistore-compress/compress`).
- [`oxistore-core`](https://crates.io/crates/oxistore-core) — provides `StoreError`, the target of the `From<CompressError>` conversion.
- [`oxistore-columnar`](https://crates.io/crates/oxistore-columnar) — uses the same OxiARC DEFLATE backend for optional payload-level Parquet compression.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)

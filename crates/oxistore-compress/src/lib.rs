//! # oxistore-compress
//!
//! Compression codec bridge for OxiStore, backed exclusively by
//! `oxiarc-deflate` (Pure Rust DEFLATE — RFC 1951).
//!
//! This crate **never** depends on `flate2`, `zstd`, `brotli`, `snap`, or
//! `miniz_oxide`.  All compression goes through the COOLJAPAN OxiARC stack.
//!
//! ## Features
//!
//! - `compress` — enables [`OxiArcCodec`] and the [`parquet::compression::Codec`]
//!   shim so that Parquet pages can be compressed with OxiARC DEFLATE.
//!
//! ## Example
//!
//! ```rust,no_run
//! # #[cfg(feature = "compress")]
//! # {
//! use oxistore_compress::OxiArcCodec;
//!
//! let codec = OxiArcCodec::new();
//! let data  = b"hello, world! ".repeat(1000);
//! let compressed   = codec.compress(&data).unwrap();
//! let decompressed = codec.decompress(&compressed).unwrap();
//! assert_eq!(decompressed, data.to_vec());
//! # }
//! ```

#[cfg(feature = "compress")]
pub mod codec;

#[cfg(feature = "compress")]
pub mod parquet_shim;

#[cfg(feature = "compress")]
pub use codec::OxiArcCodec;

/// Errors produced by [`OxiArcCodec`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressError {
    /// Compression failure — wraps the underlying error message.
    Compress(String),
    /// Decompression failure — wraps the underlying error message.
    Decompress(String),
    /// The requested compression level is out of the valid range (0–9).
    InvalidLevel(u32),
}

impl core::fmt::Display for CompressError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CompressError::Compress(msg) => write!(f, "compress error: {msg}"),
            CompressError::Decompress(msg) => write!(f, "decompress error: {msg}"),
            CompressError::InvalidLevel(level) => {
                write!(f, "invalid compression level {level}: valid range is 0–9")
            }
        }
    }
}

impl std::error::Error for CompressError {}

// ── From<CompressError> for StoreError ────────────────────────────────────────

impl From<CompressError> for oxistore_core::StoreError {
    fn from(e: CompressError) -> Self {
        oxistore_core::StoreError::Other(e.to_string())
    }
}

// ── From<OxiArcError> ─────────────────────────────────────────────────────────

#[cfg(feature = "compress")]
impl From<oxiarc_core::error::OxiArcError> for CompressError {
    fn from(e: oxiarc_core::error::OxiArcError) -> Self {
        // Route every OxiArcError to the appropriate CompressError variant by
        // inspecting the display string.  We cannot pattern-match on I/O errors
        // inside the enum directly (they are not Clone), so we use Display.
        let msg = e.to_string();
        // OxiArcError variants related to decompression are emitted during
        // inflate; everything else is treated as a compression-side error.
        CompressError::Decompress(msg)
    }
}

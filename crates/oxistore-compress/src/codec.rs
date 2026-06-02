//! [`OxiArcCodec`] — DEFLATE codec backed by `oxiarc-deflate`.
//!
//! # Policy
//!
//! This codec **exclusively** uses `oxiarc_deflate::{deflate, inflate}`.
//! It never links `flate2`, `zstd`, `brotli`, `snap`, or `miniz_oxide`.

use crate::CompressError;
use oxiarc_deflate::{deflate, inflate};

/// Default DEFLATE compression level (balanced speed/ratio).
const DEFAULT_LEVEL: u8 = 6;
/// Maximum valid DEFLATE compression level.
const MAX_LEVEL: u32 = 9;

/// Pure-Rust DEFLATE compression codec backed by OxiARC (`oxiarc-deflate`).
///
/// `OxiArcCodec` is cheaply copyable — it carries no state.
#[derive(Debug, Clone, Copy, Default)]
pub struct OxiArcCodec {
    level: u8,
}

impl OxiArcCodec {
    /// Create a codec using the default compression level (6 — balanced).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            level: DEFAULT_LEVEL,
        }
    }

    /// Create a codec with an explicit compression level (0 = store, 9 = best).
    ///
    /// Values above 9 are clamped to 9.
    #[must_use]
    pub const fn with_level(level: u8) -> Self {
        let clamped = if level > 9 { 9 } else { level };
        Self { level: clamped }
    }

    /// Create a codec with an explicit compression level, returning an error for
    /// values outside the valid range 0–9.
    ///
    /// # Errors
    ///
    /// Returns [`CompressError::InvalidLevel`] if `level > 9`.
    pub fn new_with_level(level: u32) -> Result<Self, CompressError> {
        if level > MAX_LEVEL {
            return Err(CompressError::InvalidLevel(level));
        }
        Ok(Self { level: level as u8 })
    }

    /// Compress `data` using DEFLATE and return the compressed bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CompressError::Compress`] if the underlying DEFLATE encoder
    /// fails.
    pub fn compress(&self, data: &[u8]) -> Result<Vec<u8>, CompressError> {
        deflate(data, self.level).map_err(|e| CompressError::Compress(e.to_string()))
    }

    /// Decompress DEFLATE-compressed `data` and return the original bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CompressError::Decompress`] if the DEFLATE stream is invalid
    /// or corrupted.
    pub fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, CompressError> {
        inflate(data).map_err(|e| CompressError::Decompress(e.to_string()))
    }

    /// Decompress DEFLATE-compressed `data`, appending the result to `out`.
    ///
    /// This avoids an extra allocation when the caller already has an output
    /// buffer.
    ///
    /// # Errors
    ///
    /// Returns [`CompressError::Decompress`] if the DEFLATE stream is invalid
    /// or corrupted.
    pub fn decompress_into(data: &[u8], out: &mut Vec<u8>) -> Result<(), CompressError> {
        let decompressed = inflate(data).map_err(|e| CompressError::Decompress(e.to_string()))?;
        out.extend_from_slice(&decompressed);
        Ok(())
    }

    /// Compress `data` with an advisory size hint.
    ///
    /// The `size_hint` parameter is advisory — it may be used by future
    /// implementations to pre-allocate the output buffer.  The current
    /// implementation ignores it and delegates to [`compress`](Self::compress).
    ///
    /// # Errors
    ///
    /// Returns [`CompressError::Compress`] if the underlying DEFLATE encoder
    /// fails.
    pub fn compress_with_hint(
        &self,
        data: &[u8],
        _size_hint: usize,
    ) -> Result<Vec<u8>, CompressError> {
        self.compress(data)
    }

    /// Return the name of the compression algorithm used by this codec.
    ///
    /// Always returns `"DEFLATE"` — this codec is exclusively backed by
    /// OxiARC DEFLATE (`oxiarc-deflate`).
    #[must_use]
    pub const fn algorithm_name(&self) -> &'static str {
        "DEFLATE"
    }

    /// Return the compression level configured for this codec, if applicable.
    ///
    /// Returns `Some(level)` where `level` is in the range 0–9.  Level 0
    /// means "store only" (no compression); level 9 means "best compression".
    #[must_use]
    pub const fn compression_level(&self) -> Option<u8> {
        Some(self.level)
    }
}

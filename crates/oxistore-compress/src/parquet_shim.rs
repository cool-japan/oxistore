//! Parquet [`Codec`](parquet::compression::Codec) implementation for
//! [`OxiArcCodec`].
//!
//! This shim bridges the Parquet compression API with our OxiARC DEFLATE
//! codec so that Parquet pages can be compressed and decompressed using
//! `oxiarc-deflate` — a Pure Rust implementation — instead of any C-backed
//! library.
//!
//! The implementation is gated by the `compress` feature which pulls in both
//! `oxiarc-deflate` **and** `parquet`.

use crate::OxiArcCodec;
use parquet::errors::ParquetError;

impl parquet::compression::Codec for OxiArcCodec {
    fn compress(
        &mut self,
        input_buf: &[u8],
        output_buf: &mut Vec<u8>,
    ) -> parquet::errors::Result<()> {
        // Use `OxiArcCodec::compress` via the inherent method, avoiding
        // recursive collision with this trait method name.
        let compressed = OxiArcCodec::compress(self, input_buf)
            .map_err(|e| ParquetError::General(e.to_string()))?;
        output_buf.extend_from_slice(&compressed);
        Ok(())
    }

    fn decompress(
        &mut self,
        input_buf: &[u8],
        output_buf: &mut Vec<u8>,
        uncompress_size: Option<usize>,
    ) -> parquet::errors::Result<usize> {
        if let Some(hint) = uncompress_size {
            output_buf.reserve(hint);
        }
        // Use `OxiArcCodec::decompress` via the inherent method.
        let decompressed = OxiArcCodec::decompress(self, input_buf)
            .map_err(|e| ParquetError::General(e.to_string()))?;
        let len = decompressed.len();
        output_buf.extend_from_slice(&decompressed);
        Ok(len)
    }
}

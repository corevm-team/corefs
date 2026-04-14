use crate::error::{CoreFsError, CoreFsResult};
use lz4_flex::frame::{FrameDecoder, FrameEncoder};
use std::io::{Read, Write};

/// Minimum payload size that benefits from compression.
/// Payloads smaller than this are stored uncompressed even when compression is enabled.
const MIN_COMPRESS_BYTES: usize = 64;

#[derive(Debug, Default)]
pub struct CompressionService;

impl CompressionService {
    /// Returns `true` when `data` is large enough for compression to be worthwhile.
    pub fn should_compress(&self, data: &[u8]) -> bool {
        data.len() >= MIN_COMPRESS_BYTES
    }

    /// Compress `data` with LZ4 frame format.
    pub fn compress(&self, data: &[u8]) -> CoreFsResult<Vec<u8>> {
        let mut encoder = FrameEncoder::new(Vec::new());
        encoder
            .write_all(data)
            .map_err(|e| CoreFsError::State(format!("compression write failed: {e}")))?;
        encoder
            .finish()
            .map_err(|e| CoreFsError::State(format!("compression finish failed: {e}")))
    }

    /// Decompress LZ4-frame-compressed `data`.
    pub fn decompress(&self, data: &[u8]) -> CoreFsResult<Vec<u8>> {
        let mut decoder = FrameDecoder::new(data);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|e| CoreFsError::State(format!("decompression failed: {e}")))?;
        Ok(out)
    }
}

#[cfg(test)]
#[path = "compression_tests.rs"]
mod tests;

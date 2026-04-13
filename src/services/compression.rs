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
mod tests {
    use super::*;

    #[test]
    fn compress_decompress_roundtrip() {
        let service = CompressionService;
        let data = b"hello corefs compression! ".repeat(100);
        let compressed = service.compress(&data).expect("compress");
        assert!(
            compressed.len() < data.len(),
            "repeated payload should compress smaller"
        );
        let restored = service.decompress(&compressed).expect("decompress");
        assert_eq!(restored, data);
    }

    #[test]
    fn small_payload_is_still_handled_correctly() {
        let service = CompressionService;
        let data = b"tiny";
        assert!(!service.should_compress(data));
        // Even below threshold we can still compress/decompress if caller insists.
        let compressed = service.compress(data).expect("compress");
        let restored = service.decompress(&compressed).expect("decompress");
        assert_eq!(restored, data);
    }
}

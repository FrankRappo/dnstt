use anyhow::Result;

/// Compress data using LZ4 (frame format).
pub fn compress(data: &[u8]) -> Vec<u8> {
    lz4_flex::compress_prepend_size(data)
}

/// Decompress LZ4-compressed data.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    lz4_flex::decompress_size_prepended(data)
        .map_err(|e| anyhow::anyhow!("lz4 decompress error: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_roundtrip() {
        let data = b"Hello World! This is test data for compression.";
        let compressed = compress(data);
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_compress_binary() {
        let data: Vec<u8> = (0..256).map(|i| i as u8).cycle().take(1000).collect();
        let compressed = compress(&data);
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_compress_empty() {
        let compressed = compress(b"");
        let decompressed = decompress(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn test_compression_ratio() {
        // Repetitive data should compress well
        let data = vec![0x42; 10000];
        let compressed = compress(&data);
        assert!(
            compressed.len() < data.len() / 2,
            "compression ratio too low: {} -> {}",
            data.len(),
            compressed.len()
        );
    }
}

/// Timing steganography: encode data in intervals between DNS queries.
/// Speed: ~10 bits/sec (1.25 bytes/sec)
/// Use cases: bootstrap signaling, key exchange, command channel.
use anyhow::Result;
use std::time::Duration;
use tokio::net::UdpSocket;

pub struct TimingStegoEncoder {
    /// Duration for bit 0
    pub zero_delay: Duration,
    /// Duration for bit 1
    pub one_delay: Duration,
}

impl TimingStegoEncoder {
    pub fn new() -> Self {
        Self {
            zero_delay: Duration::from_millis(50),
            one_delay: Duration::from_millis(100),
        }
    }

    /// Encode a byte array into a sequence of delays.
    pub fn encode(&self, data: &[u8]) -> Vec<Duration> {
        let mut delays = Vec::new();
        for byte in data {
            for bit_pos in (0..8).rev() {
                let bit = (byte >> bit_pos) & 1;
                if bit == 0 {
                    delays.push(self.zero_delay);
                } else {
                    delays.push(self.one_delay);
                }
            }
        }
        delays
    }

    /// Send data using timing steganography.
    /// Sends normal-looking DNS queries with encoded timing.
    pub async fn send(
        &self,
        data: &[u8],
        resolver_ip: &str,
        domain: &str,
    ) -> Result<()> {
        let delays = self.encode(data);

        eprintln!(
            "[stego] Sending {} bytes ({} bits) via timing...",
            data.len(),
            delays.len()
        );

        for (i, delay) in delays.iter().enumerate() {
            // Wait the encoded delay
            tokio::time::sleep(*delay).await;

            // Send a normal-looking DNS query (content doesn't matter, only timing)
            let domain_name = format!("stg-{}-{:04x}.{}", i, rand::random::<u16>(), domain);
            let query = sp_transport::build_dns_query(&domain_name)?;

            let socket = UdpSocket::bind("0.0.0.0:0").await?;
            socket.connect(format!("{}:53", resolver_ip)).await?;
            socket.send(&query.wire).await?;
            // Don't wait for response — fire and forget
        }

        eprintln!("[stego] Sent {} bits", delays.len());
        Ok(())
    }
}

/// Decoder for timing steganography.
/// Measures intervals between incoming queries on the server side.
pub struct TimingStegoDecoder {
    pub zero_delay: Duration,
    pub one_delay: Duration,
}

impl TimingStegoDecoder {
    pub fn new() -> Self {
        Self {
            zero_delay: Duration::from_millis(50),
            one_delay: Duration::from_millis(100),
        }
    }

    /// Decode a sequence of measured intervals into bytes.
    pub fn decode(&self, intervals: &[Duration]) -> Vec<u8> {
        let threshold = (self.zero_delay + self.one_delay) / 2;

        let bits: Vec<u8> = intervals
            .iter()
            .map(|interval| if *interval < threshold { 0u8 } else { 1u8 })
            .collect();

        // Convert bits to bytes
        let mut bytes = Vec::new();
        for chunk in bits.chunks(8) {
            if chunk.len() == 8 {
                let byte = chunk
                    .iter()
                    .enumerate()
                    .fold(0u8, |acc, (i, &bit)| acc | (bit << (7 - i)));
                bytes.push(byte);
            }
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stego_encode_decode_roundtrip() {
        let encoder = TimingStegoEncoder::new();
        let decoder = TimingStegoDecoder::new();

        let data = b"Hello";
        let delays = encoder.encode(data);
        assert_eq!(delays.len(), 40); // 5 bytes * 8 bits

        let decoded = decoder.decode(&delays);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_stego_single_byte() {
        let encoder = TimingStegoEncoder::new();
        let decoder = TimingStegoDecoder::new();

        let data = &[0b10101010];
        let delays = encoder.encode(data);
        assert_eq!(delays.len(), 8);

        // Check alternating pattern
        assert_eq!(delays[0], Duration::from_millis(100)); // 1
        assert_eq!(delays[1], Duration::from_millis(50)); // 0
        assert_eq!(delays[2], Duration::from_millis(100)); // 1
        assert_eq!(delays[3], Duration::from_millis(50)); // 0

        let decoded = decoder.decode(&delays);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_stego_all_zeros() {
        let encoder = TimingStegoEncoder::new();
        let decoder = TimingStegoDecoder::new();

        let data = &[0x00, 0x00];
        let delays = encoder.encode(data);
        assert_eq!(delays.len(), 16);
        assert!(delays.iter().all(|d| *d == Duration::from_millis(50)));

        let decoded = decoder.decode(&delays);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_stego_all_ones() {
        let encoder = TimingStegoEncoder::new();
        let decoder = TimingStegoDecoder::new();

        let data = &[0xFF, 0xFF];
        let delays = encoder.encode(data);
        assert_eq!(delays.len(), 16);
        assert!(delays.iter().all(|d| *d == Duration::from_millis(100)));

        let decoded = decoder.decode(&delays);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_stego_partial_byte_ignored() {
        let decoder = TimingStegoDecoder::new();

        // 10 intervals = 1 full byte + 2 extra bits (ignored)
        let intervals = vec![
            Duration::from_millis(100), // 1
            Duration::from_millis(50),  // 0
            Duration::from_millis(100), // 1
            Duration::from_millis(50),  // 0
            Duration::from_millis(100), // 1
            Duration::from_millis(50),  // 0
            Duration::from_millis(100), // 1
            Duration::from_millis(50),  // 0
            // extra bits — not a full byte
            Duration::from_millis(100),
            Duration::from_millis(100),
        ];

        let decoded = decoder.decode(&intervals);
        assert_eq!(decoded, vec![0b10101010]);
    }
}

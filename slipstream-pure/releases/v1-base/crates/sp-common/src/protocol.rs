use bytes::{Buf, BufMut, Bytes, BytesMut};
use rand::Rng;

/// Session ID length in bytes (displayed as hex = 16 chars)
const SESSION_ID_BYTES: usize = 8;

/// Generate a random session ID (hex string, lowercase).
pub fn generate_session_id() -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..SESSION_ID_BYTES).map(|_| rng.gen()).collect();
    hex_encode(&bytes)
}

fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Tunnel packet header.
///
/// Wire format:
/// ```text
/// [1 byte: packet_type] [2 bytes: stream_id] [4 bytes: seq_num] [2 bytes: payload_len] [payload]
/// ```
#[derive(Debug, Clone)]
pub struct TunnelPacket {
    pub packet_type: PacketType,
    pub stream_id: u16,
    pub seq_num: u32,
    pub payload: Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    /// QUIC packet data
    Data = 0x01,
    /// Keepalive / polling for downstream data
    Poll = 0x02,
    /// Session establishment
    Hello = 0x03,
    /// Session teardown
    Bye = 0x04,
}

impl TunnelPacket {
    pub fn new_data(stream_id: u16, seq_num: u32, payload: Bytes) -> Self {
        Self {
            packet_type: PacketType::Data,
            stream_id,
            seq_num,
            payload,
        }
    }

    pub fn new_poll(stream_id: u16, seq_num: u32) -> Self {
        Self {
            packet_type: PacketType::Poll,
            stream_id,
            seq_num,
            payload: Bytes::new(),
        }
    }

    pub fn new_hello(session_id: &str) -> Self {
        Self {
            packet_type: PacketType::Hello,
            stream_id: 0,
            seq_num: 0,
            payload: Bytes::from(session_id.to_string()),
        }
    }

    /// Serialize to bytes for DNS transport.
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(9 + self.payload.len());
        buf.put_u8(self.packet_type as u8);
        buf.put_u16(self.stream_id);
        buf.put_u32(self.seq_num);
        buf.put_u16(self.payload.len() as u16);
        buf.put_slice(&self.payload);
        buf.freeze()
    }

    /// Deserialize from bytes.
    pub fn decode(mut data: &[u8]) -> anyhow::Result<Self> {
        if data.len() < 9 {
            anyhow::bail!("packet too short: {} bytes", data.len());
        }
        let type_byte = data.get_u8();
        let packet_type = match type_byte {
            0x01 => PacketType::Data,
            0x02 => PacketType::Poll,
            0x03 => PacketType::Hello,
            0x04 => PacketType::Bye,
            _ => anyhow::bail!("unknown packet type: 0x{:02x}", type_byte),
        };
        let stream_id = data.get_u16();
        let seq_num = data.get_u32();
        let payload_len = data.get_u16() as usize;
        if data.len() < payload_len {
            anyhow::bail!(
                "payload truncated: expected {} got {}",
                payload_len,
                data.len()
            );
        }
        let payload = Bytes::copy_from_slice(&data[..payload_len]);

        Ok(Self {
            packet_type,
            stream_id,
            seq_num,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id() {
        let id = generate_session_id();
        assert_eq!(id.len(), SESSION_ID_BYTES * 2);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_packet_roundtrip() {
        let pkt = TunnelPacket::new_data(42, 1337, Bytes::from_static(b"hello"));
        let encoded = pkt.encode();
        let decoded = TunnelPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.packet_type, PacketType::Data);
        assert_eq!(decoded.stream_id, 42);
        assert_eq!(decoded.seq_num, 1337);
        assert_eq!(decoded.payload, Bytes::from_static(b"hello"));
    }

    #[test]
    fn test_packet_poll() {
        let pkt = TunnelPacket::new_poll(0, 100);
        let encoded = pkt.encode();
        let decoded = TunnelPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.packet_type, PacketType::Poll);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn test_packet_too_short() {
        assert!(TunnelPacket::decode(&[0x01, 0x00]).is_err());
    }
}

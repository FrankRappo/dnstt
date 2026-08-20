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

/// Generate a random group ID (same format as session ID).
pub fn generate_group_id() -> String {
    generate_session_id()
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
    /// Application data
    Data = 0x01,
    /// Keepalive / polling for downstream data
    Poll = 0x02,
    /// Session establishment
    Hello = 0x03,
    /// Session teardown
    Bye = 0x04,
    /// Connect to destination (payload: [2 bytes port BE][host UTF-8])
    Connect = 0x05,
    /// Stream closed / connection failed
    Disconnect = 0x06,
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

    /// Create a Hello packet with adaptive sizing hints for the server.
    /// Server uses max_raw/chunk_size to optimize downstream batching per session.
    pub fn new_hello_adaptive(session_id: &str, max_raw: usize, chunk_size: usize) -> Self {
        let payload = format!("{} max_raw={} chunk={}", session_id, max_raw, chunk_size);
        Self {
            packet_type: PacketType::Hello,
            stream_id: 0,
            seq_num: 0,
            payload: Bytes::from(payload),
        }
    }

    /// Create a Hello packet with group info for N sub-streams.
    /// Format: "session_id group=GID sub=N total=T max_raw=NNNN chunk=NNNN"
    pub fn new_hello_group(
        session_id: &str, group_id: &str, sub: usize, total: usize,
        max_raw: usize, chunk_size: usize,
    ) -> Self {
        let payload = format!(
            "{} group={} sub={} total={} max_raw={} chunk={}",
            session_id, group_id, sub, total, max_raw, chunk_size,
        );
        Self {
            packet_type: PacketType::Hello,
            stream_id: 0,
            seq_num: 0,
            payload: Bytes::from(payload),
        }
    }

    /// Create a Connect packet requesting the server to connect to host:port.
    /// Payload format: [2 bytes: port big-endian][host UTF-8 bytes]
    pub fn new_connect(stream_id: u16, host: &str, port: u16) -> Self {
        let mut payload = BytesMut::with_capacity(2 + host.len());
        payload.put_u16(port);
        payload.put_slice(host.as_bytes());
        Self {
            packet_type: PacketType::Connect,
            stream_id,
            seq_num: 0,
            payload: payload.freeze(),
        }
    }

    /// Create a Disconnect packet to signal stream closure.
    pub fn new_disconnect(stream_id: u16) -> Self {
        Self {
            packet_type: PacketType::Disconnect,
            stream_id,
            seq_num: 0,
            payload: Bytes::new(),
        }
    }

    /// Parse host and port from a Connect packet payload.
    pub fn parse_connect_payload(&self) -> Option<(String, u16)> {
        if self.packet_type != PacketType::Connect || self.payload.len() < 3 {
            return None;
        }
        let port = u16::from_be_bytes([self.payload[0], self.payload[1]]);
        let host = String::from_utf8_lossy(&self.payload[2..]).to_string();
        Some((host, port))
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
            0x05 => PacketType::Connect,
            0x06 => PacketType::Disconnect,
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

    /// Decode multiple concatenated packets from a buffer.
    pub fn decode_all(data: &[u8]) -> Vec<Self> {
        let mut packets = Vec::new();
        let mut offset = 0;
        while offset + 9 <= data.len() {
            match Self::decode(&data[offset..]) {
                Ok(packet) => {
                    offset += 9 + packet.payload.len();
                    packets.push(packet);
                }
                Err(_) => break,
            }
        }
        packets
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

    #[test]
    fn test_connect_packet() {
        let pkt = TunnelPacket::new_connect(7, "example.com", 443);
        let encoded = pkt.encode();
        let decoded = TunnelPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.packet_type, PacketType::Connect);
        assert_eq!(decoded.stream_id, 7);
        let (host, port) = decoded.parse_connect_payload().unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn test_disconnect_packet() {
        let pkt = TunnelPacket::new_disconnect(3);
        let encoded = pkt.encode();
        let decoded = TunnelPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.packet_type, PacketType::Disconnect);
        assert_eq!(decoded.stream_id, 3);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn test_decode_all() {
        let p1 = TunnelPacket::new_data(1, 0, Bytes::from_static(b"aaa"));
        let p2 = TunnelPacket::new_data(2, 1, Bytes::from_static(b"bbb"));
        let p3 = TunnelPacket::new_disconnect(3);
        let mut combined = Vec::new();
        combined.extend_from_slice(&p1.encode());
        combined.extend_from_slice(&p2.encode());
        combined.extend_from_slice(&p3.encode());

        let decoded = TunnelPacket::decode_all(&combined);
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].stream_id, 1);
        assert_eq!(decoded[0].payload, Bytes::from_static(b"aaa"));
        assert_eq!(decoded[1].stream_id, 2);
        assert_eq!(decoded[1].payload, Bytes::from_static(b"bbb"));
        assert_eq!(decoded[2].packet_type, PacketType::Disconnect);
        assert_eq!(decoded[2].stream_id, 3);
    }
}

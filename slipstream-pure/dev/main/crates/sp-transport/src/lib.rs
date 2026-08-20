pub mod dns_udp;
pub mod dns_tcp;
pub mod doh;
pub mod doh_ech;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use sp_common::config::TransportType;

/// DNS query/response pair for tunnel communication.
#[derive(Debug, Clone)]
pub struct DnsQuery {
    /// The full DNS query name (e.g., `<data>.session.tunnel.example.com`)
    pub qname: String,
    /// Raw DNS wire-format query packet
    pub wire: Bytes,
}

/// DNS response from a resolver.
#[derive(Debug, Clone)]
pub struct DnsResponse {
    /// Raw DNS wire-format response
    pub wire: Bytes,
    /// Extracted TXT record data (decoded from the response)
    pub txt_data: Option<Vec<u8>>,
}

/// Unified transport interface for sending DNS queries through different channels.
#[async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Transport type identifier
    fn transport_type(&self) -> TransportType;

    /// Send a DNS query and receive the response.
    async fn exchange(&self, query: &DnsQuery) -> Result<DnsResponse>;

    /// Check if this transport is currently usable.
    async fn is_available(&self) -> bool {
        true
    }

    /// Human-readable description (for logging)
    fn description(&self) -> String;
}

/// Build a raw DNS query packet (wire format) for an A record query.
pub fn build_dns_query(qname: &str) -> Result<DnsQuery> {
    use hickory_proto::op::{Edns, Message, MessageType, OpCode, Query};
    use hickory_proto::rr::{Name, RecordType};
    use hickory_proto::serialize::binary::BinEncodable;

    let name: Name = qname
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid DNS name '{}': {}", qname, e))?;

    let mut message = Message::new();
    message.set_id(rand::random::<u16>());
    message.set_message_type(MessageType::Query);
    message.set_op_code(OpCode::Query);
    message.set_recursion_desired(true);

    let mut query = Query::new();
    query.set_name(name);
    query.set_query_type(RecordType::A);
    message.add_query(query);

    // Add EDNS0 OPT for larger responses
    let edns = message.extensions_mut().get_or_insert_with(Edns::new);
    edns.set_max_payload(sp_common::EDNS0_PAYLOAD_SIZE);
    edns.set_version(0);

    let wire = message
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("DNS encode error: {}", e))?;

    Ok(DnsQuery {
        qname: qname.to_string(),
        wire: Bytes::from(wire),
    })
}

/// Build a DNS query for a TXT record (used for downstream polling).
pub fn build_dns_txt_query(qname: &str) -> Result<DnsQuery> {
    use hickory_proto::op::{Edns, Message, MessageType, OpCode, Query};
    use hickory_proto::rr::{Name, RecordType};
    use hickory_proto::serialize::binary::BinEncodable;

    let name: Name = qname
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid DNS name '{}': {}", qname, e))?;

    let mut message = Message::new();
    message.set_id(rand::random::<u16>());
    message.set_message_type(MessageType::Query);
    message.set_op_code(OpCode::Query);
    message.set_recursion_desired(true);

    let mut query = Query::new();
    query.set_name(name);
    query.set_query_type(RecordType::TXT);
    message.add_query(query);

    let edns = message.extensions_mut().get_or_insert_with(Edns::new);
    edns.set_max_payload(sp_common::EDNS0_PAYLOAD_SIZE);
    edns.set_version(0);

    let wire = message
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("DNS encode error: {}", e))?;

    Ok(DnsQuery {
        qname: qname.to_string(),
        wire: Bytes::from(wire),
    })
}

/// Parse a DNS response and extract TXT record data.
pub fn parse_dns_response(wire: &[u8]) -> Result<DnsResponse> {
    use hickory_proto::op::Message;
    use hickory_proto::rr::RData;
    use hickory_proto::serialize::binary::BinDecodable;

    let message = Message::from_bytes(wire)
        .map_err(|e| anyhow::anyhow!("DNS response parse error: {}", e))?;

    let mut txt_data = None;
    for answer in message.answers() {
        if let RData::TXT(txt) = answer.data() {
            let mut combined = Vec::new();
            for s in txt.txt_data() {
                combined.extend_from_slice(s);
            }
            txt_data = Some(combined);
            break;
        }
    }

    Ok(DnsResponse {
        wire: Bytes::copy_from_slice(wire),
        txt_data,
    })
}

/// Build a DNS TXT query with tunnel data in EDNS0 OPT record.
/// QNAME = `<session_id>.<domain>` (no data labels).
/// Tunnel data goes in a custom EDNS0 option (code 65001).
pub fn build_dns_txt_query_with_edns0(session_id: &str, domain: &str, tunnel_data: &[u8]) -> Result<DnsQuery> {
    use hickory_proto::op::{Edns, Message, MessageType, OpCode, Query};
    use hickory_proto::rr::{Name, RecordType};
    use hickory_proto::rr::rdata::opt::EdnsOption;
    use hickory_proto::serialize::binary::BinEncodable;

    let qname = format!("{}.{}", session_id, domain);
    let name: Name = qname
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid DNS name '{}': {}", qname, e))?;

    let mut message = Message::new();
    message.set_id(rand::random::<u16>());
    message.set_message_type(MessageType::Query);
    message.set_op_code(OpCode::Query);
    message.set_recursion_desired(true);

    let mut query = Query::new();
    query.set_name(name);
    query.set_query_type(RecordType::TXT);
    message.add_query(query);

    let edns = message.extensions_mut().get_or_insert_with(Edns::new);
    edns.set_max_payload(sp_common::EDNS0_PAYLOAD_SIZE);
    edns.set_version(0);
    edns.options_mut().insert(EdnsOption::Unknown(
        sp_common::EDNS0_TUNNEL_OPTION,
        tunnel_data.to_vec(),
    ));

    let wire = message
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("DNS encode error: {}", e))?;

    Ok(DnsQuery {
        qname,
        wire: Bytes::from(wire),
    })
}

/// Extract tunnel data from EDNS0 OPT record in a DNS query.
/// Returns None if no custom tunnel option found.
pub fn extract_edns0_tunnel_data(query_wire: &[u8]) -> Option<Vec<u8>> {
    use hickory_proto::op::Message;
    use hickory_proto::rr::rdata::opt::{EdnsCode, EdnsOption};
    use hickory_proto::serialize::binary::BinDecodable;

    let message = Message::from_bytes(query_wire).ok()?;
    let edns = match message.extensions() {
        Some(edns) => edns,
        None => return None,
    };
    let opt = edns.options().get(EdnsCode::Unknown(sp_common::EDNS0_TUNNEL_OPTION))?;
    match opt {
        EdnsOption::Unknown(_, data) if !data.is_empty() => Some(data.clone()),
        _ => None,
    }
}

/// Build a DNS response with TXT record data (for server).
pub fn build_dns_response(query_wire: &[u8], txt_payload: &[u8]) -> Result<Bytes> {
    use hickory_proto::op::Message;
    use hickory_proto::rr::rdata::TXT;
    use hickory_proto::rr::{RData, Record};
    use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};

    let query = Message::from_bytes(query_wire)
        .map_err(|e| anyhow::anyhow!("parse query error: {}", e))?;

    let mut response = Message::new();
    response.set_id(query.id());
    response.set_message_type(hickory_proto::op::MessageType::Response);
    response.set_op_code(hickory_proto::op::OpCode::Query);
    response.set_authoritative(true);
    response.set_recursion_desired(query.recursion_desired());
    response.set_recursion_available(false);

    for q in query.queries() {
        response.add_query(q.clone());
    }

    if let Some(q) = query.queries().first() {
        // Split payload into 255-byte chunks (TXT record string limit)
        let chunks: Vec<String> = txt_payload
            .chunks(255)
            .map(|c| String::from_utf8_lossy(c).to_string())
            .collect();

        let txt = TXT::new(chunks);
        let record = Record::from_rdata(q.name().clone(), 0, RData::TXT(txt));
        response.add_answer(record);
    }

    let wire = response
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("DNS response encode error: {}", e))?;

    Ok(Bytes::from(wire))
}

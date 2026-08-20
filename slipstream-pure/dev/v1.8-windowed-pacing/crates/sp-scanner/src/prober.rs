use anyhow::Result;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tracing::debug;

/// Result of probing a single resolver.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProbeResult {
    pub ip: String,
    pub alive: bool,
    pub delegation_works: bool,
    pub latency_ms: Option<u64>,
    pub method: ProbeMethod,
    pub real_resolver_ip: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMethod {
    Doh,
    Udp,
}

impl std::fmt::Display for ProbeMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeMethod::Doh => write!(f, "DoH"),
            ProbeMethod::Udp => write!(f, "UDP"),
        }
    }
}

/// Probe a resolver for liveness. Try DoH first if URL available, then UDP.
pub async fn probe_resolver(
    ip: &str,
    doh_url: Option<&str>,
    domain: &str,
) -> ProbeResult {
    // Try DoH first (stealth)
    if let Some(url) = doh_url {
        match probe_doh(url, domain).await {
            Ok((alive, latency)) if alive => {
                return ProbeResult {
                    ip: ip.to_string(),
                    alive: true,
                    delegation_works: false,
                    latency_ms: Some(latency),
                    method: ProbeMethod::Doh,
                    real_resolver_ip: None,
                    error: None,
                };
            }
            Err(e) => {
                debug!("DoH probe failed for {} ({}): {}", ip, url, e);
            }
            _ => {}
        }
    }

    // Fallback: UDP probe
    match probe_udp(ip, domain).await {
        Ok((alive, latency)) if alive => {
            ProbeResult {
                ip: ip.to_string(),
                alive: true,
                delegation_works: false,
                latency_ms: Some(latency),
                method: ProbeMethod::Udp,
                real_resolver_ip: None,
                error: None,
            }
        }
        Ok(_) => ProbeResult {
            ip: ip.to_string(),
            alive: false,
            delegation_works: false,
            latency_ms: None,
            method: ProbeMethod::Udp,
            real_resolver_ip: None,
            error: Some("no response".to_string()),
        },
        Err(e) => ProbeResult {
            ip: ip.to_string(),
            alive: false,
            delegation_works: false,
            latency_ms: None,
            method: ProbeMethod::Udp,
            real_resolver_ip: None,
            error: Some(e.to_string()),
        },
    }
}

/// Test delegation through a resolver by sending probe-<random>.domain query.
/// Returns the real resolver IP from the server's response.
pub async fn test_delegation(
    ip: &str,
    doh_url: Option<&str>,
    domain: &str,
) -> DelegationResult {
    let probe_id: u32 = rand::random();
    let probe_name = format!("probe-{:08x}.{}", probe_id, domain);

    // Try DoH first
    if let Some(url) = doh_url {
        match delegation_doh(url, &probe_name).await {
            Ok(result) => return result,
            Err(e) => debug!("DoH delegation test failed for {} ({}): {}", ip, url, e),
        }
    }

    // Fallback: UDP
    match delegation_udp(ip, &probe_name).await {
        Ok(result) => result,
        Err(e) => DelegationResult {
            works: false,
            real_resolver_ip: None,
            latency_ms: None,
            method: ProbeMethod::Udp,
            error: Some(e.to_string()),
        },
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DelegationResult {
    pub works: bool,
    pub real_resolver_ip: Option<String>,
    pub latency_ms: Option<u64>,
    pub method: ProbeMethod,
    pub error: Option<String>,
}

/// Send a DNS A query via UDP to test if resolver is alive.
async fn probe_udp(ip: &str, domain: &str) -> Result<(bool, u64)> {
    let qname = format!("ping.{}", domain);
    let query = sp_transport::build_dns_query(&qname)?;

    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let addr = format!("{}:53", ip);
    socket.connect(&addr).await?;

    let start = Instant::now();
    socket.send(&query.wire).await?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("timeout"))??;

    let latency = start.elapsed().as_millis() as u64;

    // Any valid DNS response means alive
    use hickory_proto::op::Message;
    use hickory_proto::serialize::binary::BinDecodable;
    let _msg = Message::from_bytes(&buf[..n])?;

    Ok((true, latency))
}

/// Send a DNS query via DoH to test if resolver is alive.
async fn probe_doh(doh_url: &str, domain: &str) -> Result<(bool, u64)> {
    let qname = format!("ping.{}", domain);
    let query = sp_transport::build_dns_query(&qname)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let start = Instant::now();
    let response = client
        .post(doh_url)
        .header("Content-Type", "application/dns-message")
        .header("Accept", "application/dns-message")
        .body(query.wire.to_vec())
        .send()
        .await?;

    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }

    let body = response.bytes().await?;
    let latency = start.elapsed().as_millis() as u64;

    // Verify it's a valid DNS response
    use hickory_proto::op::Message;
    use hickory_proto::serialize::binary::BinDecodable;
    let _msg = Message::from_bytes(&body)?;

    Ok((true, latency))
}

/// Test delegation via UDP — send TXT query for probe subdomain.
async fn delegation_udp(ip: &str, probe_name: &str) -> Result<DelegationResult> {
    let query = sp_transport::build_dns_txt_query(probe_name)?;

    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let addr = format!("{}:53", ip);
    socket.connect(&addr).await?;

    let start = Instant::now();
    socket.send(&query.wire).await?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("timeout"))??;

    let latency = start.elapsed().as_millis() as u64;
    let response = sp_transport::parse_dns_response(&buf[..n])?;

    if let Some(txt_data) = &response.txt_data {
        let txt = String::from_utf8_lossy(txt_data);
        if let Some(resolver_ip) = parse_probe_response(&txt) {
            return Ok(DelegationResult {
                works: true,
                real_resolver_ip: Some(resolver_ip),
                latency_ms: Some(latency),
                method: ProbeMethod::Udp,
                error: None,
            });
        }
    }

    Ok(DelegationResult {
        works: false,
        real_resolver_ip: None,
        latency_ms: Some(latency),
        method: ProbeMethod::Udp,
        error: Some("no probe response in TXT".to_string()),
    })
}

/// Test delegation via DoH — send TXT query for probe subdomain.
async fn delegation_doh(doh_url: &str, probe_name: &str) -> Result<DelegationResult> {
    let query = sp_transport::build_dns_txt_query(probe_name)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let start = Instant::now();
    let resp = client
        .post(doh_url)
        .header("Content-Type", "application/dns-message")
        .header("Accept", "application/dns-message")
        .body(query.wire.to_vec())
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }

    let body = resp.bytes().await?;
    let latency = start.elapsed().as_millis() as u64;

    let response = sp_transport::parse_dns_response(&body)?;

    if let Some(txt_data) = &response.txt_data {
        let txt = String::from_utf8_lossy(txt_data);
        if let Some(resolver_ip) = parse_probe_response(&txt) {
            return Ok(DelegationResult {
                works: true,
                real_resolver_ip: Some(resolver_ip),
                latency_ms: Some(latency),
                method: ProbeMethod::Doh,
                error: None,
            });
        }
    }

    Ok(DelegationResult {
        works: false,
        real_resolver_ip: None,
        latency_ms: Some(latency),
        method: ProbeMethod::Doh,
        error: Some("no probe response in TXT".to_string()),
    })
}

/// Parse "resolver=<ip>;time=<ts>;probe=<id>" from probe TXT response.
fn parse_probe_response(txt: &str) -> Option<String> {
    for part in txt.split(';') {
        let part = part.trim();
        if let Some(ip) = part.strip_prefix("resolver=") {
            return Some(ip.to_string());
        }
    }
    None
}

/// Build a DNS query for a specific record type (for validation).
pub fn build_dns_typed_query(qname: &str, rtype: hickory_proto::rr::RecordType) -> Result<sp_transport::DnsQuery> {
    use hickory_proto::op::{Edns, Message, MessageType, OpCode, Query};
    use hickory_proto::rr::Name;
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
    query.set_query_type(rtype);
    message.add_query(query);

    let edns = message.extensions_mut().get_or_insert_with(Edns::new);
    edns.set_max_payload(sp_common::EDNS0_PAYLOAD_SIZE);
    edns.set_version(0);

    let wire = message
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("DNS encode error: {}", e))?;

    Ok(sp_transport::DnsQuery {
        qname: qname.to_string(),
        wire: bytes::Bytes::from(wire),
    })
}

/// Build a DNS query with a specific EDNS0 buffer size.
pub fn build_dns_query_with_edns_size(qname: &str, edns_size: u16) -> Result<sp_transport::DnsQuery> {
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
    edns.set_max_payload(edns_size);
    edns.set_version(0);

    let wire = message
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("DNS encode error: {}", e))?;

    Ok(sp_transport::DnsQuery {
        qname: qname.to_string(),
        wire: bytes::Bytes::from(wire),
    })
}

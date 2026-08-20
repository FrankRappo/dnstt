use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use sp_common::config::TransportType;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

use crate::{DnsQuery, DnsResponse, Transport, parse_dns_response};

/// DNS-over-UDP transport. Sends raw DNS queries to a resolver on port 53.
pub struct DnsUdpTransport {
    resolver_addr: SocketAddr,
    resolver_name: String,
}

impl DnsUdpTransport {
    pub fn new(resolver_ip: &str, resolver_name: &str) -> Result<Self> {
        let addr: SocketAddr = format!("{}:53", resolver_ip)
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid resolver IP '{}': {}", resolver_ip, e))?;
        Ok(Self {
            resolver_addr: addr,
            resolver_name: resolver_name.to_string(),
        })
    }
}

#[async_trait]
impl Transport for DnsUdpTransport {
    fn transport_type(&self) -> TransportType {
        TransportType::DnsUdp
    }

    async fn exchange(&self, query: &DnsQuery) -> Result<DnsResponse> {
        // Bind to any available local port
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect(self.resolver_addr).await?;

        // Send DNS query
        socket.send(&query.wire).await?;

        // Receive response with timeout
        let mut buf = vec![0u8; 4096];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            socket.recv(&mut buf),
        )
        .await
        .map_err(|_| anyhow::anyhow!("DNS UDP timeout from {}", self.resolver_name))?
        .map_err(|e| anyhow::anyhow!("DNS UDP recv error from {}: {}", self.resolver_name, e))?;

        parse_dns_response(&buf[..n])
    }

    fn description(&self) -> String {
        format!("DNS-UDP/{} ({})", self.resolver_name, self.resolver_addr)
    }
}

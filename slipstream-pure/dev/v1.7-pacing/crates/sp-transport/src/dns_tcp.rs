use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use sp_common::config::TransportType;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{DnsQuery, DnsResponse, Transport, parse_dns_response};

/// DNS-over-TCP transport. Sends DNS queries over TCP with 2-byte length prefix.
pub struct DnsTcpTransport {
    resolver_addr: SocketAddr,
    resolver_name: String,
}

impl DnsTcpTransport {
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
impl Transport for DnsTcpTransport {
    fn transport_type(&self) -> TransportType {
        TransportType::DnsTcp
    }

    async fn exchange(&self, query: &DnsQuery) -> Result<DnsResponse> {
        let mut stream = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            TcpStream::connect(self.resolver_addr),
        )
        .await
        .map_err(|_| anyhow::anyhow!("DNS TCP connect timeout to {}", self.resolver_name))?
        .map_err(|e| {
            anyhow::anyhow!("DNS TCP connect error to {}: {}", self.resolver_name, e)
        })?;

        // DNS TCP: 2-byte length prefix + query
        let len = query.wire.len() as u16;
        stream.write_all(&len.to_be_bytes()).await?;
        stream.write_all(&query.wire).await?;
        stream.flush().await?;

        // Read response: 2-byte length prefix + response
        let resp_len = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut len_buf = [0u8; 2];
            stream.read_exact(&mut len_buf).await?;
            Ok::<u16, std::io::Error>(u16::from_be_bytes(len_buf))
        })
        .await
        .map_err(|_| anyhow::anyhow!("DNS TCP response timeout from {}", self.resolver_name))?
        .map_err(|e| {
            anyhow::anyhow!("DNS TCP read length error from {}: {}", self.resolver_name, e)
        })?;

        let mut resp_buf = vec![0u8; resp_len as usize];
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            stream.read_exact(&mut resp_buf),
        )
        .await
        .map_err(|_| anyhow::anyhow!("DNS TCP response body timeout from {}", self.resolver_name))?
        .map_err(|e| {
            anyhow::anyhow!(
                "DNS TCP read body error from {}: {}",
                self.resolver_name,
                e
            )
        })?;

        parse_dns_response(&resp_buf)
    }

    fn description(&self) -> String {
        format!("DNS-TCP/{} ({})", self.resolver_name, self.resolver_addr)
    }
}

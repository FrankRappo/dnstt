use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use sp_common::config::TransportType;

use crate::{DnsQuery, DnsResponse, Transport, parse_dns_response};

/// DNS-over-HTTPS with Encrypted Client Hello (ECH) support.
///
/// Currently uses standard DoH — ECH will be activated when rustls
/// gains stable ECH support. The transport is structured to allow
/// seamless upgrade.
pub struct DohEchTransport {
    doh_url: String,
    resolver_name: String,
    client: reqwest::Client,
}

impl DohEchTransport {
    pub fn new(doh_url: &str, resolver_name: &str) -> Result<Self> {
        // TODO: Configure ECH when rustls supports it.
        // For now, this behaves like standard DoH but is a separate
        // transport type so the multipath router can distinguish it.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| anyhow::anyhow!("HTTP client build error: {}", e))?;

        Ok(Self {
            doh_url: doh_url.to_string(),
            resolver_name: resolver_name.to_string(),
            client,
        })
    }
}

#[async_trait]
impl Transport for DohEchTransport {
    fn transport_type(&self) -> TransportType {
        TransportType::DohEch
    }

    async fn exchange(&self, query: &DnsQuery) -> Result<DnsResponse> {
        let response = self
            .client
            .post(&self.doh_url)
            .header("Content-Type", "application/dns-message")
            .header("Accept", "application/dns-message")
            .body(query.wire.to_vec())
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!("DoH+ECH request error to {}: {}", self.resolver_name, e)
            })?;

        if !response.status().is_success() {
            anyhow::bail!(
                "DoH+ECH HTTP error from {}: {}",
                self.resolver_name,
                response.status()
            );
        }

        let body = response.bytes().await.map_err(|e| {
            anyhow::anyhow!(
                "DoH+ECH response read error from {}: {}",
                self.resolver_name,
                e
            )
        })?;

        parse_dns_response(&body)
    }

    fn description(&self) -> String {
        format!("DoH+ECH/{} ({})", self.resolver_name, self.doh_url)
    }
}

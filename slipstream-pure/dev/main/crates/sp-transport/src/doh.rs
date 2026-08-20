use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use sp_common::config::TransportType;

use crate::{DnsQuery, DnsResponse, Transport, parse_dns_response};

/// DNS-over-HTTPS transport. Sends DNS queries via HTTPS POST (RFC 8484).
pub struct DohTransport {
    doh_url: String,
    resolver_name: String,
    client: reqwest::Client,
}

impl DohTransport {
    pub fn new(doh_url: &str, resolver_name: &str) -> Result<Self> {
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
impl Transport for DohTransport {
    fn transport_type(&self) -> TransportType {
        TransportType::Doh
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
                anyhow::anyhow!("DoH request error to {}: {}", self.resolver_name, e)
            })?;

        if !response.status().is_success() {
            anyhow::bail!(
                "DoH HTTP error from {}: {}",
                self.resolver_name,
                response.status()
            );
        }

        let body = response.bytes().await.map_err(|e| {
            anyhow::anyhow!("DoH response read error from {}: {}", self.resolver_name, e)
        })?;

        parse_dns_response(&body)
    }

    async fn is_available(&self) -> bool {
        // DoH requires HTTPS connectivity
        self.client.get(&self.doh_url).send().await.is_ok()
    }

    fn description(&self) -> String {
        format!("DoH/{} ({})", self.resolver_name, self.doh_url)
    }
}

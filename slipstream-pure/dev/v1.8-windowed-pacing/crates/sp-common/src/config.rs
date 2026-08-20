use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Top-level config from resolvers.toml
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(rename = "resolver")]
    pub resolvers: Vec<ResolverConfig>,
}

/// A single DNS resolver configuration
#[derive(Debug, Clone, Deserialize)]
pub struct ResolverConfig {
    pub name: String,
    pub ip: Vec<String>,
    #[serde(default)]
    pub doh: Option<String>,
    #[serde(default)]
    pub ech: bool,
    /// Max EDNS0 UDP payload size this resolver supports (from scanner validate)
    #[serde(default)]
    pub edns0_max: Option<u16>,
    /// Max queries per second before rate limiting (from scanner profile)
    #[serde(default)]
    pub rate_limit_rps: Option<u32>,
}

impl Config {
    /// Load config from a TOML file
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| "failed to parse resolvers.toml")?;
        Ok(config)
    }

    /// Get all resolver IPs as flat list of (name, ip, resolver_config) tuples
    pub fn all_endpoints(&self) -> Vec<ResolverEndpoint> {
        let mut endpoints = Vec::new();
        for resolver in &self.resolvers {
            for ip in &resolver.ip {
                endpoints.push(ResolverEndpoint {
                    name: resolver.name.clone(),
                    ip: ip.clone(),
                    doh_url: resolver.doh.clone(),
                    ech_supported: resolver.ech,
                    edns0_max: resolver.edns0_max,
                    rate_limit_rps: resolver.rate_limit_rps,
                });
            }
        }
        endpoints
    }
}

/// A single resolver endpoint (one IP from a resolver)
#[derive(Debug, Clone)]
pub struct ResolverEndpoint {
    pub name: String,
    pub ip: String,
    pub doh_url: Option<String>,
    pub ech_supported: bool,
    /// Max EDNS0 UDP payload size (from scanner)
    pub edns0_max: Option<u16>,
    /// Max queries per second (from scanner)
    pub rate_limit_rps: Option<u32>,
}

impl ResolverEndpoint {
    /// Returns the best transport type available for this endpoint
    pub fn best_transport(&self) -> TransportType {
        if self.ech_supported && self.doh_url.is_some() {
            TransportType::DohEch
        } else if self.doh_url.is_some() {
            TransportType::Doh
        } else {
            TransportType::DnsUdp
        }
    }

    /// Returns all available transport types, best first
    pub fn available_transports(&self) -> Vec<TransportType> {
        let mut transports = Vec::new();
        if self.ech_supported && self.doh_url.is_some() {
            transports.push(TransportType::DohEch);
        }
        if self.doh_url.is_some() {
            transports.push(TransportType::Doh);
        }
        transports.push(TransportType::DnsTcp);
        transports.push(TransportType::DnsUdp);
        transports
    }
}

/// Transport type for DNS tunnel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportType {
    DnsUdp,
    DnsTcp,
    Doh,
    DohEch,
}

impl ResolverEndpoint {
    /// Compute optimal downstream batch size (MAX_RAW) based on EDNS0 max payload.
    /// Returns the max raw data bytes that fit in a DNS TXT response for this resolver.
    /// Accounts for DNS header (~40B), TXT record overhead (~20B), batch_id prefix (4B).
    /// Falls back to conservative 1000 if edns0_max is unknown.
    pub fn optimal_max_raw(&self) -> usize {
        match self.edns0_max {
            Some(edns) => {
                let edns = edns as usize;
                // DNS response overhead: ~12 header + ~(qname) + answer RR header ~30 + EDNS OPT ~11
                // TXT record: each 255-byte string has 1-byte length prefix
                // Conservative: subtract 100 bytes for all DNS overhead
                let usable = edns.saturating_sub(100);
                // TXT encoding: every 255 bytes of payload costs 1 byte for length prefix
                // So effective payload = usable * 255/256 ≈ usable - usable/256
                let effective = usable.saturating_sub(usable / 256);
                // Subtract 4 bytes for batch_id prefix
                let max_raw = effective.saturating_sub(4);
                // Clamp to reasonable range: 200..3500
                max_raw.clamp(200, 3500)
            }
            None => 1000, // conservative default (current hardcoded value)
        }
    }

    /// Compute optimal downstream chunk size for tcp_read buffer.
    /// Each chunk becomes one TunnelPacket (9B header + payload).
    /// Multiple chunks should fit in one batch (max_raw).
    /// Target: 2 chunks per batch for good throughput.
    pub fn optimal_chunk_size(&self) -> usize {
        let max_raw = self.optimal_max_raw();
        // TunnelPacket overhead: 9 bytes (1 type + 2 stream_id + 4 seq + 2 payload_len)
        // Target: 2 packets per batch → (max_raw - overhead) / 2
        let chunk = (max_raw.saturating_sub(18)) / 2;
        chunk.clamp(200, 1500)
    }
}

impl std::fmt::Display for TransportType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportType::DnsUdp => write!(f, "DNS-UDP"),
            TransportType::DnsTcp => write!(f, "DNS-TCP"),
            TransportType::Doh => write!(f, "DoH"),
            TransportType::DohEch => write!(f, "DoH+ECH"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_resolvers_toml() {
        let toml_str = r#"
[[resolver]]
name = "Cloudflare"
ip = ["1.1.1.1", "1.0.0.1"]
doh = "https://cloudflare-dns.com/dns-query"
ech = true

[[resolver]]
name = "Google"
ip = ["8.8.8.8"]
doh = "https://dns.google/dns-query"
ech = false

[[resolver]]
name = "Level3"
ip = ["4.2.2.1", "4.2.2.2"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.resolvers.len(), 3);
        assert_eq!(config.resolvers[0].name, "Cloudflare");
        assert_eq!(config.resolvers[0].ip.len(), 2);
        assert!(config.resolvers[0].ech);
        assert!(!config.resolvers[1].ech);
        assert!(config.resolvers[2].doh.is_none());

        let endpoints = config.all_endpoints();
        assert_eq!(endpoints.len(), 5); // 2 + 1 + 2
        assert_eq!(endpoints[0].best_transport(), TransportType::DohEch);
        assert_eq!(endpoints[2].best_transport(), TransportType::Doh);
        assert_eq!(endpoints[3].best_transport(), TransportType::DnsUdp);
    }
}

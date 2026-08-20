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

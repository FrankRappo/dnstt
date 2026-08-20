use anyhow::Result;
use rand::Rng;
use rand::seq::SliceRandom;
use std::time::Duration;
use tokio::net::UdpSocket;

/// Generates legitimate-looking DNS queries mixed with real scanner probes.
/// Makes scanner traffic look like normal browsing.
pub struct DecoyTrafficGenerator {
    domains: Vec<&'static str>,
    rng: rand::rngs::StdRng,
}

impl DecoyTrafficGenerator {
    pub fn new() -> Self {
        use rand::SeedableRng;
        Self {
            domains: vec![
                "google.com",
                "youtube.com",
                "facebook.com",
                "amazon.com",
                "wikipedia.org",
                "twitter.com",
                "instagram.com",
                "linkedin.com",
                "reddit.com",
                "netflix.com",
                "microsoft.com",
                "apple.com",
                "github.com",
                "stackoverflow.com",
                "yahoo.com",
                "bing.com",
                "whatsapp.com",
                "zoom.us",
                "office.com",
                "live.com",
                "cloudflare.com",
                "tiktok.com",
                "pinterest.com",
                "ebay.com",
                "twitch.tv",
                "discord.com",
                "spotify.com",
                "telegram.org",
            ],
            rng: rand::rngs::StdRng::from_entropy(),
        }
    }

    /// Send a decoy DNS query to a resolver (looks like normal browsing).
    pub async fn send_decoy(&mut self, resolver_ip: &str) -> Result<()> {
        let domain = *self.domains.choose(&mut self.rng).unwrap();
        let query = sp_transport::build_dns_query(domain)?;

        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket
            .connect(format!("{}:53", resolver_ip))
            .await?;
        socket.send(&query.wire).await?;

        // Fire and forget — don't wait for response
        Ok(())
    }

    /// Returns a random popular domain name.
    pub fn random_domain(&mut self) -> &str {
        self.domains.choose(&mut self.rng).unwrap()
    }
}

/// Mimics browser DNS query patterns: bursts then silence.
pub struct TrafficShaper {
    min_delay_ms: u64,
    max_delay_ms: u64,
    burst_probability: f64,
    #[allow(dead_code)]
    burst_size: usize,
    burst_silence_ms: u64,
    rng: rand::rngs::StdRng,
}

impl TrafficShaper {
    pub fn browser_like() -> Self {
        use rand::SeedableRng;
        Self {
            min_delay_ms: 30,
            max_delay_ms: 200,
            burst_probability: 0.3,
            burst_size: 5,
            burst_silence_ms: 2000,
            rng: rand::rngs::StdRng::from_entropy(),
        }
    }

    #[allow(dead_code)]
    pub fn aggressive() -> Self {
        use rand::SeedableRng;
        Self {
            min_delay_ms: 5,
            max_delay_ms: 20,
            burst_probability: 0.0,
            burst_size: 1,
            burst_silence_ms: 0,
            rng: rand::rngs::StdRng::from_entropy(),
        }
    }

    /// Returns the delay before the next query (in ms).
    /// Simulates browser patterns: sometimes bursts of quick queries
    /// (like loading a page), sometimes longer pauses.
    pub fn next_delay_ms(&mut self) -> u64 {
        if self.rng.gen_bool(self.burst_probability) {
            // Burst mode: short delay (within a "page load")
            self.rng.gen_range(5..30)
        } else {
            // Normal: random delay in range
            self.rng.gen_range(self.min_delay_ms..=self.max_delay_ms)
        }
    }

    /// Returns true if we should insert a silence pause (between "page loads").
    pub fn should_pause(&mut self) -> bool {
        self.burst_silence_ms > 0 && self.rng.gen_bool(0.05)
    }

    /// Duration of a silence pause.
    pub fn pause_duration_ms(&mut self) -> u64 {
        self.rng.gen_range(500..=self.burst_silence_ms)
    }
}

/// Generate DNS query names that look like normal CDN/API traffic
/// instead of obvious base32-encoded tunnel data.
pub struct QnameCamouflage {
    prefixes: Vec<&'static str>,
    suffixes: Vec<&'static str>,
    rng: rand::rngs::StdRng,
}

impl QnameCamouflage {
    pub fn new() -> Self {
        use rand::SeedableRng;
        Self {
            prefixes: vec![
                "cdn-cache",
                "static-asset",
                "img",
                "api-v2",
                "media",
                "edge",
                "origin",
                "content",
                "assets",
                "dist",
                "js",
                "css",
                "fonts",
                "data",
                "config",
                "tile",
                "map",
                "geo",
                "loc",
                "srv",
            ],
            suffixes: vec![
                "static", "cdn", "media", "assets", "content", "edge", "cache", "origin", "dist",
                "api",
            ],
            rng: rand::rngs::StdRng::from_entropy(),
        }
    }

    /// Generate a camouflaged subdomain that encodes data but looks like CDN traffic.
    /// Format: `<prefix>-<hex_data>.<suffix>.<domain>`
    /// Example: `cdn-cache-a7f3c9.static.tunnel.example.com`
    pub fn encode(&mut self, data: &[u8], domain: &str) -> String {
        let prefix = *self.prefixes.choose(&mut self.rng).unwrap();
        let suffix = *self.suffixes.choose(&mut self.rng).unwrap();

        // Hex-encode data manually (avoids extra dependency)
        let hex: String = data.iter().map(|b| format!("{:02x}", b)).collect();

        // Split hex into label-sized chunks (max 63 chars per DNS label)
        let max_hex_per_label = 63 - prefix.len() - 1; // -1 for the dash
        let chunks: Vec<&str> = hex
            .as_bytes()
            .chunks(max_hex_per_label)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();

        let mut name = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            if i == 0 {
                name.push_str(&format!("{}-{}", prefix, chunk));
            } else {
                name.push_str(&format!(".{}-{}", prefix, chunk));
            }
        }
        name.push_str(&format!(".{}.{}", suffix, domain));
        name
    }

    /// Generate a completely normal-looking subdomain for decoy queries.
    pub fn decoy_name(&mut self, domain: &str) -> String {
        let decoys = vec![
            "www", "mail", "ns1", "ns2", "mx", "ftp", "webmail", "portal",
        ];
        let sub = decoys.choose(&mut self.rng).unwrap();
        format!("{}.{}", sub, domain)
    }
}

/// Mix query types to avoid fingerprinting.
/// Normal users query mostly A and AAAA, rarely TXT.
/// If we only query TXT → suspicious.
pub struct QueryTypeMixer {
    rng: rand::rngs::StdRng,
}

impl QueryTypeMixer {
    pub fn new() -> Self {
        use rand::SeedableRng;
        Self {
            rng: rand::rngs::StdRng::from_entropy(),
        }
    }

    /// Returns the record type to use for this probe.
    /// Distribution mimics normal browsing: 40% A, 25% AAAA, 15% HTTPS, 10% TXT, 10% DNSKEY
    pub fn next_type(&mut self) -> hickory_proto::rr::RecordType {
        use hickory_proto::rr::RecordType;

        let roll: f64 = self.rng.gen();
        if roll < 0.40 {
            RecordType::A
        } else if roll < 0.65 {
            RecordType::AAAA
        } else if roll < 0.80 {
            RecordType::Unknown(65) // HTTPS (type 65), very common in browsers now
        } else if roll < 0.90 {
            RecordType::TXT
        } else {
            RecordType::DNSKEY
        }
    }

    /// Returns a good record type for maximum data capacity.
    #[allow(dead_code)]
    pub fn best_capacity_type() -> hickory_proto::rr::RecordType {
        hickory_proto::rr::RecordType::TXT
    }
}

/// Wraps scanner with stealth features: decoy traffic, traffic shaping, QNAME camo.
pub struct StealthScanner {
    pub decoy: DecoyTrafficGenerator,
    pub shaper: TrafficShaper,
    pub camo: QnameCamouflage,
    pub query_mixer: QueryTypeMixer,
    /// Ratio of decoy to real queries (0.0-1.0). 0.3 = 30% decoy.
    pub decoy_ratio: f32,
    /// Number of decoy queries sent so far.
    pub decoy_count: u64,
    /// Sum of all delays applied (for avg calculation).
    pub total_delay_ms: u64,
    /// Number of delays applied.
    pub delay_count: u64,
}

impl StealthScanner {
    pub fn new(decoy_ratio: f32) -> Self {
        Self {
            decoy: DecoyTrafficGenerator::new(),
            shaper: TrafficShaper::browser_like(),
            camo: QnameCamouflage::new(),
            query_mixer: QueryTypeMixer::new(),
            decoy_ratio,
            decoy_count: 0,
            total_delay_ms: 0,
            delay_count: 0,
        }
    }

    /// Perform a stealthy probe: apply traffic shaping, optionally send decoy.
    pub async fn stealth_probe(
        &mut self,
        ip: &str,
        doh_url: Option<&str>,
        domain: &str,
    ) -> crate::prober::ProbeResult {
        // Traffic shaping delay
        let delay = self.shaper.next_delay_ms();
        if delay > 0 {
            self.total_delay_ms += delay;
            self.delay_count += 1;
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        // Occasional longer pause (simulates user reading page)
        if self.shaper.should_pause() {
            let pause = self.shaper.pause_duration_ms();
            self.total_delay_ms += pause;
            tokio::time::sleep(Duration::from_millis(pause)).await;
        }

        // Maybe send a decoy first
        let mut rng = rand::thread_rng();
        if rng.gen_bool(self.decoy_ratio as f64) {
            let _ = self.decoy.send_decoy(ip).await;
            self.decoy_count += 1;
            let gap: u64 = rng.gen_range(10..50);
            tokio::time::sleep(Duration::from_millis(gap)).await;
        }

        // Real probe
        crate::prober::probe_resolver(ip, doh_url, domain).await
    }

    /// Perform a stealthy delegation test with decoys mixed in.
    pub async fn stealth_delegation(
        &mut self,
        ip: &str,
        doh_url: Option<&str>,
        domain: &str,
    ) -> crate::prober::DelegationResult {
        let delay = self.shaper.next_delay_ms();
        if delay > 0 {
            self.total_delay_ms += delay;
            self.delay_count += 1;
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        let mut rng = rand::thread_rng();
        if rng.gen_bool(self.decoy_ratio as f64) {
            let _ = self.decoy.send_decoy(ip).await;
            self.decoy_count += 1;
            let gap: u64 = rng.gen_range(10..50);
            tokio::time::sleep(Duration::from_millis(gap)).await;
        }

        crate::prober::test_delegation(ip, doh_url, domain).await
    }

    /// Average delay in ms across all shaped queries.
    pub fn avg_delay_ms(&self) -> u64 {
        if self.delay_count == 0 {
            0
        } else {
            self.total_delay_ms / self.delay_count
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoy_generator_has_domains() {
        let gen = DecoyTrafficGenerator::new();
        assert!(!gen.domains.is_empty());
    }

    #[test]
    fn test_traffic_shaper_delays() {
        let mut shaper = TrafficShaper::browser_like();
        for _ in 0..100 {
            let delay = shaper.next_delay_ms();
            assert!(delay <= 2000); // should never exceed burst_silence_ms
        }
    }

    #[test]
    fn test_qname_camouflage_format() {
        let mut camo = QnameCamouflage::new();
        let name = camo.encode(b"\x01\x02\x03", "tunnel.example.com");
        assert!(name.ends_with(".tunnel.example.com"));
        assert!(name.contains('-')); // has prefix-hex format
        // Should NOT look like base32
        assert!(!name.contains("=="));
    }

    #[test]
    fn test_qname_camouflage_decoy() {
        let mut camo = QnameCamouflage::new();
        let name = camo.decoy_name("tunnel.example.com");
        assert!(name.ends_with(".tunnel.example.com"));
    }

    #[test]
    fn test_query_type_mixer_distribution() {
        let mut mixer = QueryTypeMixer::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..1000 {
            let t = mixer.next_type();
            *counts.entry(format!("{:?}", t)).or_insert(0) += 1;
        }
        // A should be most common (~40%)
        assert!(counts.get("A").unwrap_or(&0) > &200);
        // At least 4 different types should appear
        assert!(counts.len() >= 4);
    }

    #[test]
    fn test_stealth_scanner_stats() {
        let scanner = StealthScanner::new(0.3);
        assert_eq!(scanner.decoy_count, 0);
        assert_eq!(scanner.avg_delay_ms(), 0);
    }

    #[test]
    fn test_traffic_shaper_aggressive() {
        let mut shaper = TrafficShaper::aggressive();
        for _ in 0..100 {
            let delay = shaper.next_delay_ms();
            assert!(delay <= 20);
        }
        // Aggressive never pauses
        for _ in 0..100 {
            assert!(!shaper.should_pause());
        }
    }

    #[test]
    fn test_qname_camouflage_long_data() {
        let mut camo = QnameCamouflage::new();
        // Test with longer data that might need multiple labels
        let data = vec![0xABu8; 100];
        let name = camo.encode(&data, "tunnel.example.com");
        assert!(name.ends_with(".tunnel.example.com"));
        // Each label must be ≤ 63 chars
        for label in name.split('.') {
            assert!(
                label.len() <= 63,
                "label '{}' is {} chars",
                label,
                label.len()
            );
        }
    }
}

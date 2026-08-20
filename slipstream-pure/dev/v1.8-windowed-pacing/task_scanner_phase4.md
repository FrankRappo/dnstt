# Task: DNS Scanner — Phase 4 (Stealth Traffic Camouflage)

## Project
`/work/vpn/slipstream-pure-v1.8-windowed-pacing/`

Phase 1-3 complete. All commands work. Now adding stealth traffic camouflage.

## Read FIRST
- `crates/sp-scanner/PLAN.md` — read Phase 7 (Record Type Probing), Phase 8c (Traffic Camouflage), Phase 8h (Timing Steganography)
- `crates/sp-scanner/src/prober.rs` — current probe functions
- `crates/sp-scanner/src/scanner.rs` — scan modes, auto_detect_mode
- `crates/sp-scanner/src/main.rs` — CLI structure

## What to build

### 1. Stealth scanning module (src/stealth.rs)

New module for traffic camouflage during scanning and tunnel operation.

#### Decoy traffic generator
```rust
/// Generates legitimate-looking DNS queries mixed with real scanner probes.
/// Makes scanner traffic look like normal browsing.
pub struct DecoyTrafficGenerator {
    domains: Vec<&'static str>,    // popular domains to query
    rng: rand::rngs::StdRng,
}

impl DecoyTrafficGenerator {
    pub fn new() -> Self {
        Self {
            domains: vec![
                "google.com", "youtube.com", "facebook.com", "amazon.com",
                "wikipedia.org", "twitter.com", "instagram.com", "linkedin.com",
                "reddit.com", "netflix.com", "microsoft.com", "apple.com",
                "github.com", "stackoverflow.com", "yahoo.com", "bing.com",
                "whatsapp.com", "zoom.us", "office.com", "live.com",
                "cloudflare.com", "tiktok.com", "pinterest.com", "ebay.com",
                "twitch.tv", "discord.com", "spotify.com", "telegram.org",
                "mail.ru", "yandex.ru", "vk.com", "ok.ru",
            ],
            rng: rand::SeedableRng::from_entropy(),
        }
    }

    /// Send a decoy DNS query to a resolver (looks like normal browsing).
    pub async fn send_decoy(&mut self, resolver_ip: &str) -> Result<()> {
        use rand::seq::SliceRandom;
        let domain = self.domains.choose(&mut self.rng).unwrap();
        let query = sp_transport::build_dns_query(domain)?;

        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect(format!("{}:53", resolver_ip)).await?;
        socket.send(&query.wire).await?;

        // Don't wait for response — fire and forget
        Ok(())
    }

    /// Returns a random popular domain name.
    pub fn random_domain(&mut self) -> &str {
        use rand::seq::SliceRandom;
        self.domains.choose(&mut self.rng).unwrap()
    }
}
```

#### Traffic shaping — browser pattern mimicry
```rust
/// Mimics browser DNS query patterns: bursts then silence.
pub struct TrafficShaper {
    /// Min delay between queries (ms)
    min_delay_ms: u64,
    /// Max delay between queries (ms)
    max_delay_ms: u64,
    /// Probability of a "burst" (0.0-1.0)
    burst_probability: f64,
    /// Queries per burst
    burst_size: usize,
    /// Silence between bursts (ms)
    burst_silence_ms: u64,
    rng: rand::rngs::StdRng,
}

impl TrafficShaper {
    pub fn browser_like() -> Self {
        Self {
            min_delay_ms: 30,
            max_delay_ms: 200,
            burst_probability: 0.3,
            burst_size: 5,
            burst_silence_ms: 2000,
            rng: rand::SeedableRng::from_entropy(),
        }
    }

    pub fn aggressive() -> Self {
        Self {
            min_delay_ms: 5,
            max_delay_ms: 20,
            burst_probability: 0.0,
            burst_size: 1,
            burst_silence_ms: 0,
            rng: rand::SeedableRng::from_entropy(),
        }
    }

    /// Returns the delay before the next query (in ms).
    /// Simulates browser patterns: sometimes bursts of quick queries
    /// (like loading a page), sometimes longer pauses.
    pub fn next_delay_ms(&mut self) -> u64 {
        use rand::Rng;

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
        use rand::Rng;
        self.burst_silence_ms > 0 && self.rng.gen_bool(0.05) // 5% chance of pause
    }

    /// Duration of a silence pause.
    pub fn pause_duration_ms(&mut self) -> u64 {
        use rand::Rng;
        self.rng.gen_range(500..=self.burst_silence_ms)
    }
}
```

#### QNAME camouflage
```rust
/// Generate DNS query names that look like normal CDN/API traffic
/// instead of obvious base32-encoded tunnel data.
pub struct QnameCamouflage {
    prefixes: Vec<&'static str>,
    suffixes: Vec<&'static str>,
    rng: rand::rngs::StdRng,
}

impl QnameCamouflage {
    pub fn new() -> Self {
        Self {
            prefixes: vec![
                "cdn-cache", "static-asset", "img", "api-v2", "media",
                "edge", "origin", "content", "assets", "dist",
                "js", "css", "fonts", "data", "config",
                "tile", "map", "geo", "loc", "srv",
            ],
            suffixes: vec![
                "static", "cdn", "media", "assets", "content",
                "edge", "cache", "origin", "dist", "api",
            ],
            rng: rand::SeedableRng::from_entropy(),
        }
    }

    /// Generate a camouflaged subdomain that encodes data but looks like CDN traffic.
    /// Format: <prefix>-<hex_data>.<suffix>.<domain>
    /// Example: cdn-cache-a7f3c9.static.tunnel.example.com
    ///          img-001b4e.media.tunnel.example.com
    pub fn encode(&mut self, data: &[u8], domain: &str) -> String {
        use rand::seq::SliceRandom;
        let prefix = self.prefixes.choose(&mut self.rng).unwrap();
        let suffix = self.suffixes.choose(&mut self.rng).unwrap();
        let hex = hex::encode(data); // add hex crate or use format!("{:02x}")

        // Split hex into label-sized chunks (max 63 chars per label)
        // prefix-hexchunk.suffix.domain
        let max_hex_per_label = 63 - prefix.len() - 1; // -1 for the dash
        let chunks: Vec<&str> = hex.as_bytes()
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
    /// Example: www.tunnel.example.com, mail.tunnel.example.com
    pub fn decoy_name(&mut self, domain: &str) -> String {
        use rand::seq::SliceRandom;
        let decoys = vec!["www", "mail", "ns1", "ns2", "mx", "ftp", "webmail", "portal"];
        let sub = decoys.choose(&mut self.rng).unwrap();
        format!("{}.{}", sub, domain)
    }
}
```

#### Stealth scan wrapper — puts it all together
```rust
/// Wraps scanner with stealth features: decoy traffic, traffic shaping, QNAME camo.
pub struct StealthScanner {
    pub decoy: DecoyTrafficGenerator,
    pub shaper: TrafficShaper,
    pub camo: QnameCamouflage,
    /// Ratio of decoy to real queries (0.0-1.0). 0.3 = 30% decoy.
    pub decoy_ratio: f32,
}

impl StealthScanner {
    pub fn new(decoy_ratio: f32) -> Self {
        Self {
            decoy: DecoyTrafficGenerator::new(),
            shaper: TrafficShaper::browser_like(),
            camo: QnameCamouflage::new(),
            decoy_ratio,
        }
    }

    /// Perform a stealthy probe: apply traffic shaping, optionally send decoy.
    pub async fn stealth_probe(
        &mut self,
        ip: &str,
        doh_url: Option<&str>,
        domain: &str,
    ) -> crate::prober::ProbeResult {
        use rand::Rng;

        // Traffic shaping delay
        let delay = self.shaper.next_delay_ms();
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        // Occasional longer pause (simulates user reading page)
        if self.shaper.should_pause() {
            let pause = self.shaper.pause_duration_ms();
            tokio::time::sleep(Duration::from_millis(pause)).await;
        }

        // Maybe send a decoy first
        let mut rng = rand::thread_rng();
        if rng.gen_bool(self.decoy_ratio as f64) {
            let _ = self.decoy.send_decoy(ip).await;
            // Small delay between decoy and real query
            tokio::time::sleep(Duration::from_millis(rng.gen_range(10..50))).await;
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
        use rand::Rng;

        let delay = self.shaper.next_delay_ms();
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        let mut rng = rand::thread_rng();
        if rng.gen_bool(self.decoy_ratio as f64) {
            let _ = self.decoy.send_decoy(ip).await;
            tokio::time::sleep(Duration::from_millis(rng.gen_range(10..50))).await;
        }

        crate::prober::test_delegation(ip, doh_url, domain).await
    }
}
```

### 2. Integrate stealth into scanner.rs

When network level >= 2 (RESTRICTED/HOSTILE), use StealthScanner instead of direct prober:

```rust
// In run_scan():
let use_stealth = level >= 2; // RESTRICTED or HOSTILE networks

if use_stealth {
    eprintln!("[stealth] Using traffic camouflage (decoy ratio: 30%, browser-like timing)");
    let mut stealth = stealth::StealthScanner::new(0.3);
    // Use stealth.stealth_probe() instead of prober::probe_resolver()
    // Sequential scanning with shaped timing (not parallel)
    for resolver in &resolvers {
        let result = stealth.stealth_probe(
            resolver.ip,
            if actual_mode == ScanMode::Stealth { resolver.doh_url } else { None },
            &config.domain,
        ).await;
        // ... handle result
    }
} else {
    // Existing parallel scanning code (unchanged)
}
```

Also add stealth option to scan CLI:
```rust
Scan {
    /// Scan mode: auto (default), stealth, or aggressive
    #[arg(long, default_value = "auto")]
    mode: String,

    /// Force stealth camouflage (decoy traffic, browser-like timing)
    #[arg(long)]
    camouflage: bool,

    /// Decoy traffic ratio 0.0-1.0 (default 0.3 = 30% decoy queries)
    #[arg(long, default_value = "0.3")]
    decoy_ratio: f32,
},
```

### 3. Query type mixing for stealth

When doing stealth probes, mix DNS query types to look natural:

```rust
/// Mix query types to avoid fingerprinting.
/// Normal users query mostly A and AAAA, rarely TXT.
/// If we only query TXT → suspicious.
pub struct QueryTypeMixer {
    rng: rand::rngs::StdRng,
}

impl QueryTypeMixer {
    pub fn new() -> Self {
        Self { rng: rand::SeedableRng::from_entropy() }
    }

    /// Returns the record type to use for this probe.
    /// Distribution mimics normal browsing: 50% A, 25% AAAA, 15% HTTPS, 10% other
    /// But all types carry our tunnel data (server responds to all).
    pub fn next_type(&mut self) -> hickory_proto::rr::RecordType {
        use rand::Rng;
        use hickory_proto::rr::RecordType;

        let roll: f64 = self.rng.gen();
        if roll < 0.40 {
            RecordType::A
        } else if roll < 0.65 {
            RecordType::AAAA
        } else if roll < 0.80 {
            RecordType::HTTPS  // type 65, very common in browsers now
        } else if roll < 0.90 {
            RecordType::TXT
        } else {
            RecordType::DNSKEY // DNSSEC, normal
        }
    }

    /// Returns a good record type for maximum data capacity.
    /// Used when stealth is less important than speed.
    pub fn best_capacity_type() -> hickory_proto::rr::RecordType {
        hickory_proto::rr::RecordType::TXT // max capacity
    }
}
```

Add this to stealth.rs and use in stealth_probe when applicable.

### 4. Stealth stats display

When stealth is active, show additional stats:

```rust
// In scan progress:
eprint!("\r[{}/{}] probed={} alive={} decoys={} avg_delay={}ms",
    scanned, total, real_probes, alive, decoy_count, avg_delay_ms);
```

### 5. Tests

Add unit tests to stealth.rs:

```rust
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
        // Delays should be in range
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
        assert!(name.contains("-")); // has prefix-hex format
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
        // All types should appear
        assert!(counts.len() >= 4);
    }
}
```

### 6. Build and test

1. `cargo build` — zero errors
2. `cargo test -p sp-scanner` — all tests pass (old + new stealth tests)
3. Test:
   - `dns-scanner scan --domain tunnel.example.com` (auto, no stealth on open network)
   - `dns-scanner scan --mode stealth --camouflage --domain tunnel.example.com` (forced stealth with camo)
   - `dns-scanner scan --mode stealth --camouflage --decoy-ratio 0.5 --domain tunnel.example.com` (50% decoy)
4. Fix any warnings
5. Commit when done

## Dependencies

You may need to add to Cargo.toml:
```toml
hex = "0.4"    # for hex encoding in QNAME camouflage
```

Or implement hex encoding manually with `format!("{:02x}", byte)` to avoid extra dependency.

## Constraints
- stealth.rs is a new module — add `mod stealth;` to main.rs
- Don't break existing scan/validate/test behavior
- Stealth features only activate when explicitly requested (--camouflage) or auto-detected (level >= 2)
- On OPEN networks, scan works exactly as before (no performance penalty)
- All async with tokio
- Use rand for randomization (already a dependency)
- Commit when done

## File changes
```
NEW:    crates/sp-scanner/src/stealth.rs     ← decoy, shaper, camo, query mixer, stealth scanner
MODIFY: crates/sp-scanner/src/main.rs        ← add mod stealth, update Scan command args
MODIFY: crates/sp-scanner/src/scanner.rs     ← integrate stealth for restricted networks
MODIFY: crates/sp-scanner/Cargo.toml         ← add hex dependency (optional)
```

# Task: DNS Scanner — Phase 5 (All Remaining Features)

## Project
`/work/vpn/slipstream-pure-v1.8-windowed-pacing/`

Phase 1-4 complete. This task implements everything remaining before client integration.

## Read FIRST
- `crates/sp-scanner/PLAN.md` — full plan, read Phase 2, 4 additions, 5 additions, 8f, 8h
- All existing src files in `crates/sp-scanner/src/`
- `crates/sp-scanner/Cargo.toml` — current dependencies

## What to build (7 items)

---

### 1. scan-doh — DoH Endpoint Discovery (new file: src/scan_doh.rs)

Find NEW DoH endpoints not in bundled list.

```rust
pub struct ScanDohConfig {
    pub domain: String,
    pub output: Option<String>,
}

pub async fn run_scan_doh(config: ScanDohConfig) -> Result<()> {
    let mut found: Vec<DohEndpoint> = Vec::new();

    // Source 1: Certificate Transparency logs via crt.sh
    eprintln!("[1/3] Searching Certificate Transparency logs (crt.sh)...");
    let ct_domains = search_ct_logs().await;
    eprintln!("  Found {} candidate domains from CT logs", ct_domains.len());

    // Source 2: Brute-force common DNS subdomains on known providers
    eprintln!("[2/3] Brute-forcing DNS subdomains...");
    let brute_domains = brute_dns_subdomains().await;
    eprintln!("  Generated {} candidates from brute-force", brute_domains.len());

    // Source 3: Combine and deduplicate
    let mut all_candidates: Vec<String> = Vec::new();
    all_candidates.extend(ct_domains);
    all_candidates.extend(brute_domains);
    all_candidates.sort();
    all_candidates.dedup();
    eprintln!("[3/3] Testing {} unique DoH candidates...", all_candidates.len());

    // Test each candidate
    let sem = Arc::new(Semaphore::new(20));
    let mut tasks = Vec::new();

    for domain_candidate in all_candidates {
        let sem = sem.clone();
        let tunnel_domain = config.domain.clone();

        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            test_doh_candidate(&domain_candidate, &tunnel_domain).await
        }));
    }

    for task in tasks {
        if let Ok(Ok(Some(endpoint))) = task.await {
            eprintln!("  + FOUND: {} ({}ms)", endpoint.url, endpoint.latency_ms);
            found.push(endpoint);
        }
    }

    eprintln!("\n=== Found {} working DoH endpoints ===", found.len());
    for ep in &found {
        eprintln!("  {} — {}ms, delegation={}", ep.url, ep.latency_ms, ep.delegation_works);
    }

    // Write output
    if let Some(path) = &config.output {
        write_doh_results(&found, path)?;
    }

    Ok(())
}

struct DohEndpoint {
    url: String,
    latency_ms: u64,
    delegation_works: bool,
}

/// Search crt.sh for certificates containing DNS-related domains.
async fn search_ct_logs() -> Vec<String> {
    let search_terms = ["dns", "doh", "resolver", "dnscrypt"];
    let mut domains = Vec::new();

    for term in &search_terms {
        // crt.sh has a simple API: https://crt.sh/?q=%25dns%25&output=json
        let url = format!("https://crt.sh/?q=%25{}%25&output=json", term);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build().unwrap();

        match client.get(&url).send().await {
            Ok(resp) => {
                if let Ok(text) = resp.text().await {
                    // Parse JSON array, extract "common_name" and "name_value" fields
                    // crt.sh returns: [{"common_name":"dns.example.com","name_value":"dns.example.com\n*.dns.example.com",...}]
                    if let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                        for entry in entries.iter().take(500) { // limit per term
                            if let Some(cn) = entry.get("common_name").and_then(|v| v.as_str()) {
                                let cn = cn.trim().trim_start_matches("*.");
                                if looks_like_doh_domain(cn) {
                                    domains.push(cn.to_string());
                                }
                            }
                            if let Some(names) = entry.get("name_value").and_then(|v| v.as_str()) {
                                for name in names.lines() {
                                    let name = name.trim().trim_start_matches("*.");
                                    if looks_like_doh_domain(name) {
                                        domains.push(name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::debug!("crt.sh search for '{}' failed: {}", term, e);
            }
        }
    }

    domains.sort();
    domains.dedup();
    domains
}

fn looks_like_doh_domain(domain: &str) -> bool {
    let keywords = ["dns", "doh", "dot", "resolver", "ns1", "ns2", "nameserver"];
    let lower = domain.to_lowercase();
    keywords.iter().any(|k| lower.contains(k))
        && !lower.contains("dnssec") // too common, not DoH
        && domain.contains('.') // must be a domain
        && domain.len() < 100 // sanity
}

/// Brute-force common DNS subdomains on well-known providers.
async fn brute_dns_subdomains() -> Vec<String> {
    let prefixes = ["dns", "doh", "dns1", "dns2", "resolver", "ns", "ns1", "ns2",
                    "public-dns", "freedns", "secure-dns", "family-dns", "adblock-dns"];

    let base_domains = [
        // Major providers likely to have DoH
        "cloudflare.com", "google.com", "quad9.net", "opendns.com",
        "adguard.com", "nextdns.io", "cleanbrowsing.org", "mullvad.net",
        "controld.com", "libredns.gr", "dns.sb",
        // ISPs and hosting
        "comcast.net", "charter.com", "cox.net", "att.com", "verizon.com",
        "t-mobile.com", "deutsche-telekom.de", "orange.fr", "bt.com",
        // Cloud providers
        "amazonaws.com", "azure.com", "oracle.com", "digitalocean.com",
        "vultr.com", "linode.com", "hetzner.com", "ovh.com",
        // Security companies
        "norton.com", "safedns.com", "yandex.ru", "baidu.com",
    ];

    let mut candidates = Vec::new();
    for prefix in &prefixes {
        for base in &base_domains {
            candidates.push(format!("{}.{}", prefix, base));
        }
    }
    candidates
}

/// Test if a domain hosts a DoH endpoint.
async fn test_doh_candidate(domain: &str, tunnel_domain: &str) -> Result<Option<DohEndpoint>> {
    let paths = ["/dns-query", "/resolve", "/dns", "/query"];

    for path in &paths {
        let url = format!("https://{}{}", domain, path);

        // Build a simple DNS query
        let query = sp_transport::build_dns_txt_query(&format!("probe-{:08x}.{}", rand::random::<u32>(), tunnel_domain))?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .danger_accept_invalid_certs(true) // some may have self-signed
            .build()?;

        let start = Instant::now();
        let resp = client
            .post(&url)
            .header("Content-Type", "application/dns-message")
            .header("Accept", "application/dns-message")
            .body(query.wire.to_vec())
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let body = r.bytes().await?;
                let latency = start.elapsed().as_millis() as u64;

                // Verify it's a valid DNS response
                if let Ok(parsed) = sp_transport::parse_dns_response(&body) {
                    let delegation = parsed.txt_data.is_some();
                    return Ok(Some(DohEndpoint {
                        url,
                        latency_ms: latency,
                        delegation_works: delegation,
                    }));
                }
            }
            _ => continue,
        }
    }

    Ok(None)
}
```

Add `serde_json = "1"` to Cargo.toml dependencies.

---

### 2. scan-dot — DoT Endpoint Discovery (new file: src/scan_dot.rs)

Find DNS-over-TLS endpoints on port 853.

```rust
pub struct ScanDotConfig {
    pub domain: String,
    pub output: Option<String>,
}

pub async fn run_scan_dot(config: ScanDotConfig) -> Result<()> {
    // Known DoT endpoints to test
    let known_dot = vec![
        ("1.1.1.1", "Cloudflare"),
        ("1.0.0.1", "Cloudflare"),
        ("8.8.8.8", "Google"),
        ("8.8.4.4", "Google"),
        ("9.9.9.9", "Quad9"),
        ("149.112.112.112", "Quad9"),
        ("94.140.14.14", "AdGuard"),
        ("94.140.15.15", "AdGuard"),
        ("185.228.168.9", "CleanBrowsing"),
        ("45.90.28.0", "NextDNS"),
        ("194.242.2.2", "Mullvad"),
        ("185.222.222.222", "DNS.SB"),
        ("45.11.45.11", "DNS.SB"),
        ("208.67.222.222", "OpenDNS"),
        ("208.67.220.220", "OpenDNS"),
        ("76.76.2.0", "ControlD"),
    ];

    eprintln!("Testing {} known DoT endpoints on port 853...\n", known_dot.len());

    let sem = Arc::new(Semaphore::new(10));
    let mut tasks = Vec::new();

    for (ip, name) in known_dot {
        let sem = sem.clone();
        let domain = config.domain.clone();

        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = test_dot_endpoint(ip, &domain).await;
            (ip, name, result)
        }));
    }

    let mut working = Vec::new();
    for task in tasks {
        if let Ok((ip, name, Ok(Some((latency, delegation))))) = task.await {
            eprintln!("  + {} ({}) — {}ms, delegation={}", ip, name, latency, delegation);
            working.push((ip.to_string(), name.to_string(), latency, delegation));
        } else if let Ok((ip, name, _)) = task.await {
            eprintln!("  - {} ({}) — not available", ip, name);
        }
    }

    eprintln!("\n=== Found {} working DoT endpoints ===", working.len());

    if let Some(path) = &config.output {
        write_dot_results(&working, path)?;
    }

    Ok(())
}

async fn test_dot_endpoint(ip: &str, domain: &str) -> Result<Option<(u64, bool)>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // TLS connection to port 853
    let connector = tokio_rustls::TlsConnector::from(Arc::new(
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertVerifier))
            .with_no_client_auth()
    ));

    let addr: std::net::SocketAddr = format!("{}:853", ip).parse()?;
    let tcp = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    ).await??;

    let server_name = rustls::pki_types::ServerName::try_from(ip.to_string())
        .unwrap_or_else(|_| rustls::pki_types::ServerName::try_from("dns.example.com".to_string()).unwrap());

    let mut tls = tokio::time::timeout(
        Duration::from_secs(5),
        connector.connect(server_name, tcp),
    ).await??;

    // DNS-over-TLS: 2-byte length prefix + DNS wire format
    let probe_name = format!("probe-{:08x}.{}", rand::random::<u32>(), domain);
    let query = sp_transport::build_dns_txt_query(&probe_name)?;
    let wire = query.wire.to_vec();
    let len = (wire.len() as u16).to_be_bytes();

    let start = Instant::now();

    tls.write_all(&len).await?;
    tls.write_all(&wire).await?;
    tls.flush().await?;

    // Read response: 2-byte length prefix + DNS wire
    let mut len_buf = [0u8; 2];
    tokio::time::timeout(Duration::from_secs(5), tls.read_exact(&mut len_buf)).await??;
    let resp_len = u16::from_be_bytes(len_buf) as usize;

    let mut resp_buf = vec![0u8; resp_len];
    tokio::time::timeout(Duration::from_secs(5), tls.read_exact(&mut resp_buf)).await??;

    let latency = start.elapsed().as_millis() as u64;

    let parsed = sp_transport::parse_dns_response(&resp_buf)?;
    let delegation = parsed.txt_data.is_some();

    Ok(Some((latency, delegation)))
}

// Simple cert verifier that accepts anything (for testing)
struct NoCertVerifier;
impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self, _: &rustls::pki_types::CertificateDer<'_>, _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>, _: &[u8], _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(&self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}
```

Add dependencies to Cargo.toml:
```toml
tokio-rustls = "0.26"
rustls = { workspace = true }
```

---

### 3. Profile enhancements (modify src/profile.rs)

Add to existing ResolverProfile struct and run_profile:

```rust
// Add fields to ResolverProfile:
pub cache_ttl_secs: Option<u64>,     // how long resolver caches
pub dnssec_validates: bool,           // does it validate DNSSEC
pub tcp_supported: bool,              // TCP port 53 works
```

#### Cache TTL detection:
```rust
async fn detect_cache_ttl(ip: &str, domain: &str) -> Option<u64> {
    // Query a unique subdomain → first query goes to auth server (TTL from server)
    // Query same subdomain again after N seconds → if response faster = cached
    let unique = format!("cachettl-{:08x}.{}", rand::random::<u32>(), domain);

    // First query (uncached)
    let q1 = sp_transport::build_dns_query(&unique).ok()?;
    let socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    socket.connect(format!("{}:53", ip)).await.ok()?;
    socket.send(&q1.wire).await.ok()?;
    let mut buf = vec![0u8; 4096];
    let _ = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await.ok()??;

    // Wait 2 seconds
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second query (should be cached)
    let start = Instant::now();
    socket.send(&q1.wire).await.ok()?;
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await.ok()?.ok()?;
    let cached_latency = start.elapsed().as_millis() as u64;

    // Parse TTL from response
    use hickory_proto::op::Message;
    use hickory_proto::serialize::binary::BinDecodable;
    let msg = Message::from_bytes(&buf[..n]).ok()?;
    let ttl = msg.answers().first().map(|a| a.ttl());

    // If response has TTL and came back fast → it's cached
    ttl.map(|t| t as u64)
}
```

#### DNSSEC validation check:
```rust
async fn check_dnssec_validation(ip: &str) -> bool {
    // Query a domain known to have DNSSEC: dnssec-failed.org
    // If resolver validates DNSSEC, it should return SERVFAIL for this broken domain
    // If resolver doesn't validate, it returns the A record anyway

    let query = sp_transport::build_dns_query("dnssec-failed.org").ok();
    if query.is_none() { return false; }
    let query = query.unwrap();

    let socket = UdpSocket::bind("0.0.0.0:0").await.ok();
    if socket.is_none() { return false; }
    let socket = socket.unwrap();

    if socket.connect(format!("{}:53", ip)).await.is_err() { return false; }
    if socket.send(&query.wire).await.is_err() { return false; }

    let mut buf = vec![0u8; 4096];
    match tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await {
        Ok(Ok(n)) => {
            use hickory_proto::op::Message;
            use hickory_proto::serialize::binary::BinDecodable;
            if let Ok(msg) = Message::from_bytes(&buf[..n]) {
                // SERVFAIL = resolver validates DNSSEC (and correctly rejected the broken record)
                msg.response_code() == hickory_proto::op::ResponseCode::ServFail
            } else {
                false
            }
        }
        _ => false,
    }
}
```

#### TCP support check:
```rust
async fn check_tcp_support(ip: &str, domain: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr: std::net::SocketAddr = match format!("{}:53", ip).parse() {
        Ok(a) => a,
        Err(_) => return false,
    };

    let mut tcp = match tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(s)) => s,
        _ => return false,
    };

    let query = match sp_transport::build_dns_query(&format!("tcp-test.{}", domain)) {
        Ok(q) => q,
        Err(_) => return false,
    };

    let wire = query.wire.to_vec();
    let len = (wire.len() as u16).to_be_bytes();

    if tcp.write_all(&len).await.is_err() { return false; }
    if tcp.write_all(&wire).await.is_err() { return false; }

    let mut len_buf = [0u8; 2];
    match tokio::time::timeout(Duration::from_secs(3), tcp.read_exact(&mut len_buf)).await {
        Ok(Ok(_)) => {
            let resp_len = u16::from_be_bytes(len_buf) as usize;
            let mut resp_buf = vec![0u8; resp_len];
            match tokio::time::timeout(Duration::from_secs(3), tcp.read_exact(&mut resp_buf)).await {
                Ok(Ok(_)) => true,
                _ => false,
            }
        }
        _ => false,
    }
}
```

Add these checks to the main profile loop, print results, add to output TOML.

---

### 4. Monitor enhancements (modify src/monitor.rs)

Add to existing monitor:

#### Reliability score:
```rust
fn calculate_reliability(history: &[CheckResult]) -> u8 {
    if history.is_empty() { return 0; }
    let deleg_ok = history.iter().filter(|r| r.delegation).count();
    ((deleg_ok as f64 / history.len() as f64) * 100.0) as u8
}
```

#### Adaptive re-scan interval:
```rust
// In monitor loop, adjust per-resolver check interval:
// reliability >= 90% → check every 2x interval (stable, no need to check often)
// reliability 50-89% → check every 1x interval (normal)
// reliability < 50% → check every 0.5x interval (unstable, check more often)
fn adaptive_interval(base_interval: u64, reliability: u8) -> u64 {
    if reliability >= 90 { base_interval * 2 }
    else if reliability >= 50 { base_interval }
    else { base_interval / 2 }
}
```

#### Persistent history to JSON file:
```rust
// Save history after each check cycle:
fn save_history(history: &HashMap<String, Vec<CheckResult>>, path: &str) -> Result<()> {
    // Serialize to JSON: { "8.8.8.8": [{"ts": 123, "alive": true, "deleg": true, "latency": 45}, ...] }
    let mut map = serde_json::Map::new();
    for (ip, checks) in history {
        let arr: Vec<serde_json::Value> = checks.iter().map(|c| {
            serde_json::json!({
                "ts": c.timestamp,
                "alive": c.alive,
                "deleg": c.delegation,
                "latency": c.latency_ms,
            })
        }).collect();
        map.insert(ip.clone(), serde_json::Value::Array(arr));
    }
    let json = serde_json::to_string_pretty(&serde_json::Value::Object(map))?;
    std::fs::write(path, json)?;
    Ok(())
}

// Load history at startup:
fn load_history(path: &str) -> HashMap<String, Vec<CheckResult>> {
    // Read JSON, parse back to HashMap
    // If file doesn't exist → return empty
    let content = match std::fs::read_to_string(path) { Ok(c) => c, Err(_) => return HashMap::new() };
    let parsed: serde_json::Value = match serde_json::from_str(&content) { Ok(v) => v, Err(_) => return HashMap::new() };
    // ... parse back to CheckResult
    let mut history = HashMap::new();
    if let Some(obj) = parsed.as_object() {
        for (ip, arr) in obj {
            if let Some(checks) = arr.as_array() {
                let results: Vec<CheckResult> = checks.iter().filter_map(|v| {
                    Some(CheckResult {
                        timestamp: v.get("ts")?.as_u64()?,
                        alive: v.get("alive")?.as_bool()?,
                        delegation: v.get("deleg")?.as_bool()?,
                        latency_ms: v.get("latency")?.as_u64(),
                    })
                }).collect();
                history.insert(ip.clone(), results);
            }
        }
    }
    history
}
```

Add `--history` flag to Monitor CLI for the JSON file path:
```rust
Monitor {
    input: String,
    #[arg(long, default_value = "600")]
    interval: u64,
    /// Path to history JSON file (persistent across restarts)
    #[arg(long, default_value = "monitor-history.json")]
    history: String,
},
```

---

### 5. scan-doq — DoQ placeholder (new file: src/scan_doq.rs)

DNS-over-QUIC is complex (needs quinn). Create a working stub that tests known endpoints:

```rust
pub struct ScanDoqConfig {
    pub domain: String,
    pub output: Option<String>,
}

pub async fn run_scan_doq(config: ScanDoqConfig) -> Result<()> {
    // DoQ uses QUIC on port 853 (same as DoT but QUIC protocol)
    // Known DoQ endpoints:
    let known_doq = vec![
        ("94.140.14.14:853", "AdGuard"),
        ("94.140.15.15:853", "AdGuard"),
        ("dns.nextdns.io:853", "NextDNS"),
        ("dns.adguard-dns.com:8853", "AdGuard-8853"),
    ];

    eprintln!("Testing {} known DoQ endpoints...", known_doq.len());
    eprintln!("Note: DoQ support is experimental\n");

    // For now, just test TCP connectivity to the port as a basic check
    // Full QUIC DNS implementation would need quinn integration
    for (addr, name) in &known_doq {
        let reachable = test_port_reachable(addr).await;
        if reachable {
            eprintln!("  + {} ({}) — port reachable", addr, name);
        } else {
            eprintln!("  - {} ({}) — not reachable", addr, name);
        }
    }

    eprintln!("\nFull DoQ protocol support requires quinn integration (future)");
    Ok(())
}

async fn test_port_reachable(addr: &str) -> bool {
    match tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::UdpSocket::bind("0.0.0.0:0"),
    ).await {
        Ok(Ok(socket)) => {
            if socket.connect(addr).await.is_err() { return false; }
            // Send a few bytes to test
            let _ = socket.send(&[0u8; 1]).await;
            true
        }
        _ => false,
    }
}
```

---

### 6. Timing steganography module (new file: src/timing_stego.rs)

Ultra-covert signaling channel. Too slow for data tunnel, useful for command channel.

```rust
/// Timing steganography: encode data in intervals between DNS queries.
/// Speed: ~10 bits/sec (1.25 bytes/sec)
/// Use cases: bootstrap signaling, key exchange, command channel.
pub struct TimingStegoEncoder {
    /// Duration for bit 0
    pub zero_delay: Duration,
    /// Duration for bit 1
    pub one_delay: Duration,
    /// Tolerance for decoding
    pub tolerance: Duration,
}

impl TimingStegoEncoder {
    pub fn new() -> Self {
        Self {
            zero_delay: Duration::from_millis(50),
            one_delay: Duration::from_millis(100),
            tolerance: Duration::from_millis(15),
        }
    }

    /// Encode a byte array into a sequence of delays.
    pub fn encode(&self, data: &[u8]) -> Vec<Duration> {
        let mut delays = Vec::new();
        for byte in data {
            for bit_pos in (0..8).rev() {
                let bit = (byte >> bit_pos) & 1;
                if bit == 0 {
                    delays.push(self.zero_delay);
                } else {
                    delays.push(self.one_delay);
                }
            }
        }
        delays
    }

    /// Send data using timing steganography.
    /// Sends normal-looking DNS queries with encoded timing.
    pub async fn send(&self, data: &[u8], resolver_ip: &str, domain: &str) -> Result<()> {
        let delays = self.encode(data);
        let decoy = crate::stealth::DecoyTrafficGenerator::new();

        eprintln!("[stego] Sending {} bytes ({} bits) via timing...", data.len(), delays.len());

        for (i, delay) in delays.iter().enumerate() {
            // Wait the encoded delay
            tokio::time::sleep(*delay).await;

            // Send a normal-looking DNS query (the content doesn't matter, only timing)
            let domain_name = format!("stg-{}-{:04x}.{}", i, rand::random::<u16>(), domain);
            let query = sp_transport::build_dns_query(&domain_name)?;

            let socket = UdpSocket::bind("0.0.0.0:0").await?;
            socket.connect(format!("{}:53", resolver_ip)).await?;
            socket.send(&query.wire).await?;
            // Don't wait for response
        }

        eprintln!("[stego] Sent {} bits", delays.len());
        Ok(())
    }
}

/// Decoder for timing steganography.
/// Measures intervals between incoming queries on the server side.
pub struct TimingStegoDecoder {
    pub zero_delay: Duration,
    pub one_delay: Duration,
    pub tolerance: Duration,
}

impl TimingStegoDecoder {
    pub fn new() -> Self {
        Self {
            zero_delay: Duration::from_millis(50),
            one_delay: Duration::from_millis(100),
            tolerance: Duration::from_millis(15),
        }
    }

    /// Decode a sequence of measured intervals into bytes.
    pub fn decode(&self, intervals: &[Duration]) -> Vec<u8> {
        let mut bits = Vec::new();
        let threshold = (self.zero_delay + self.one_delay) / 2;

        for interval in intervals {
            if *interval < threshold {
                bits.push(0u8);
            } else {
                bits.push(1u8);
            }
        }

        // Convert bits to bytes
        let mut bytes = Vec::new();
        for chunk in bits.chunks(8) {
            if chunk.len() == 8 {
                let byte = chunk.iter().enumerate().fold(0u8, |acc, (i, &bit)| {
                    acc | (bit << (7 - i))
                });
                bytes.push(byte);
            }
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stego_encode_decode_roundtrip() {
        let encoder = TimingStegoEncoder::new();
        let decoder = TimingStegoDecoder::new();

        let data = b"Hello";
        let delays = encoder.encode(data);
        assert_eq!(delays.len(), 40); // 5 bytes * 8 bits

        let decoded = decoder.decode(&delays);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_stego_single_byte() {
        let encoder = TimingStegoEncoder::new();
        let decoder = TimingStegoDecoder::new();

        let data = &[0b10101010];
        let delays = encoder.encode(data);
        assert_eq!(delays.len(), 8);

        // Check alternating pattern
        assert_eq!(delays[0], Duration::from_millis(100)); // 1
        assert_eq!(delays[1], Duration::from_millis(50));  // 0
        assert_eq!(delays[2], Duration::from_millis(100)); // 1
        assert_eq!(delays[3], Duration::from_millis(50));  // 0

        let decoded = decoder.decode(&delays);
        assert_eq!(decoded, data);
    }
}
```

---

### 7. Update CLI (main.rs)

Add new subcommands:

```rust
/// Discover new DoH endpoints (CT logs, subdomain brute-force)
ScanDoh,

/// Discover DoT endpoints on port 853
ScanDot,

/// Discover DoQ endpoints (experimental)
ScanDoq,
```

Add to match:
```rust
Commands::ScanDoh => {
    scan_doh::run_scan_doh(scan_doh::ScanDohConfig {
        domain: cli.domain,
        output: cli.output,
    }).await?;
}
Commands::ScanDot => {
    scan_dot::run_scan_dot(scan_dot::ScanDotConfig {
        domain: cli.domain,
        output: cli.output,
    }).await?;
}
Commands::ScanDoq => {
    scan_doq::run_scan_doq(scan_doq::ScanDoqConfig {
        domain: cli.domain,
        output: cli.output,
    }).await?;
}
```

Add module declarations:
```rust
mod scan_doh;
mod scan_dot;
mod scan_doq;
mod timing_stego;
```

Update Monitor command to include history arg.

---

## Dependencies to add (Cargo.toml)

```toml
serde_json = "1"
tokio-rustls = "0.26"
```

Note: `rustls` is already in workspace dependencies. You may need to check version compatibility
with tokio-rustls. If tokio-rustls 0.26 doesn't match, try 0.25 or use `rustls = { workspace = true }`.

## Build and test

1. `cargo build` — zero errors
2. `cargo test -p sp-scanner` — all tests pass (existing + new stego tests)
3. Test commands:
   - `dns-scanner scan-doh --domain tunnel.example.com` (searches CT logs, brute subdomains)
   - `dns-scanner scan-dot --domain tunnel.example.com` (tests known DoT endpoints)
   - `dns-scanner scan-doq --domain tunnel.example.com` (tests known DoQ endpoints)
   - `dns-scanner profile /tmp/test-profile.txt --domain tunnel.example.com` (should show new fields: cache TTL, DNSSEC, TCP)
   - `dns-scanner monitor /tmp/test-profile.txt --interval 10 --history /tmp/test-history.json --domain tunnel.example.com` (run 2 cycles, check JSON file written)
4. Fix any warnings
5. Commit when done

## Constraints
- Reuse prober.rs and existing modules where possible
- DoT implementation needs TLS — use tokio-rustls with NoCertVerifier for testing
- DoQ is stub only (port check) — full QUIC needs quinn which is complex
- CT log search (crt.sh) may be slow — set 30s timeout, limit results
- serde_json only for monitor history and CT log parsing
- Timing stego is standalone module, not wired into tunnel yet (future)
- Don't break existing functionality
- Commit when done

## File changes
```
NEW:    crates/sp-scanner/src/scan_doh.rs       ← DoH endpoint discovery
NEW:    crates/sp-scanner/src/scan_dot.rs       ← DoT endpoint discovery
NEW:    crates/sp-scanner/src/scan_doq.rs       ← DoQ stub
NEW:    crates/sp-scanner/src/timing_stego.rs   ← timing steganography encoder/decoder
MODIFY: crates/sp-scanner/src/main.rs           ← add new commands + modules
MODIFY: crates/sp-scanner/src/profile.rs        ← add cache TTL, DNSSEC, TCP checks
MODIFY: crates/sp-scanner/src/monitor.rs        ← add reliability score, adaptive interval, persistent history
MODIFY: crates/sp-scanner/Cargo.toml            ← add serde_json, tokio-rustls
```

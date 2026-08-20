# Task: DNS Scanner — Phase 3 (Auto Mode + Profile + Monitor)

## Project
`/work/vpn/slipstream-pure-v1.8-windowed-pacing/`

Phase 1 (scan/validate/test) and Phase 2 (bootstrap/detect/hints) are complete.

## Read FIRST
- `crates/sp-scanner/PLAN.md` — full plan, read Phase 4 (Profiling), Phase 5 (Monitoring)
- `crates/sp-scanner/src/scanner.rs` — current scan logic with ScanMode::Stealth/Aggressive
- `crates/sp-scanner/src/detect.rs` — NetworkReport struct and analyze_network()
- `crates/sp-scanner/src/prober.rs` — probe/delegation functions to reuse
- `crates/sp-scanner/src/main.rs` — CLI structure
- `crates/sp-scanner/src/profile.rs` — placeholder
- `crates/sp-scanner/src/monitor.rs` — placeholder

## What to build

### 1. Auto scan mode — automatic network detection + mode selection

Update ScanMode enum and CLI:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    Auto,       // NEW: detect network, choose automatically
    Stealth,
    Aggressive,
}
```

Update CLI default from "stealth" to "auto":
```rust
Scan {
    /// Scan mode: auto (default), stealth, or aggressive
    #[arg(long, default_value = "auto")]
    mode: String,
},
```

Implement auto-detection in scanner.rs:

```rust
/// Determine scan mode based on network analysis.
/// Returns (mode, level, score) where:
///   level 0 = OPEN (score 8-10): aggressive ok
///   level 1 = FILTERED (score 5-7): stealth with fast system DNS
///   level 2 = RESTRICTED (score 3-4): stealth only
///   level 3 = HOSTILE (score 0-2): max stealth + DoH only
pub async fn auto_detect_mode(domain: &str) -> (ScanMode, u8, u8) {
    use crate::detect;

    eprintln!("[auto] Detecting network type...");
    let report = detect::analyze_network(domain).await;

    let mut score: u8 = 0;

    // Port 53 UDP reachable to external DNS?
    if report.port53_udp { score += 3; }
    // Port 53 TCP reachable?
    if report.port53_tcp { score += 1; }
    // No transparent proxy?
    if !report.transparent_proxy { score += 2; }
    // No DNS hijacking?
    if !report.dns_hijack { score += 2; }
    // Port 853 (DoT) reachable?
    if report.port853_tls { score += 1; }
    // No captive portal?
    if !report.captive_portal { score += 1; }

    let (level, mode) = if score >= 8 {
        (0, ScanMode::Aggressive)
    } else if score >= 5 {
        (1, ScanMode::Stealth)   // stealth but can use system DNS at higher rate
    } else if score >= 3 {
        (2, ScanMode::Stealth)
    } else {
        (3, ScanMode::Stealth)   // max stealth, DoH only
    };

    let level_name = match level {
        0 => "OPEN",
        1 => "FILTERED",
        2 => "RESTRICTED",
        _ => "HOSTILE",
    };

    eprintln!("[auto] Network: {} (level {}, score {}/10)", level_name, level, score);
    eprintln!("[auto] Selected mode: {:?}", mode);
    if level >= 2 {
        eprintln!("[auto] Restricted network — using DoH-only scanning");
    }

    (mode, level, score)
}
```

Update run_scan to handle Auto:
```rust
pub async fn run_scan(config: ScanConfig) -> Result<()> {
    let (actual_mode, level, score) = match config.mode {
        ScanMode::Auto => auto_detect_mode(&config.domain).await,
        other => (other, 0, 10), // forced mode, assume open
    };

    // For HOSTILE networks (level 3): only use DoH, skip all UDP probes
    let skip_udp = level >= 3;

    // Rest of scan logic uses actual_mode...
}
```

### 2. Profile command (profile.rs)

`dns-scanner profile` — deep analysis of each resolver.

```rust
pub struct ProfileConfig {
    pub input: String,        // resolvers-validated.toml or alive.txt
    pub domain: String,
    pub output: Option<String>,
}

pub struct ResolverProfile {
    pub ip: String,
    pub name: String,
    pub doh_url: Option<String>,

    // Rate limit
    pub rate_limit_rps: u32,          // max requests/sec without drops
    pub rate_limit_method: String,    // how we determined it

    // Latency
    pub latency_avg_ms: u64,
    pub latency_min_ms: u64,
    pub latency_max_ms: u64,
    pub latency_jitter_ms: u64,      // max - min

    // EDNS0
    pub edns0_max: u16,

    // Record types
    pub supported_types: Vec<String>,
    pub best_type: String,

    // Fingerprint
    pub software: Option<String>,     // BIND, Unbound, PowerDNS, etc.
    pub is_forwarder: bool,
    pub upstream_resolvers: Vec<String>,

    // Reliability
    pub delegation_works: bool,
    pub response_manipulation: bool,  // modifies answers?
    pub dnssec_validates: bool,

    // Honeypot indicators
    pub honeypot_score: u8,           // 0 = safe, 100 = definitely honeypot
}
```

#### Rate limit detection:
```rust
async fn detect_rate_limit(ip: &str, doh_url: Option<&str>, domain: &str) -> u32 {
    // Gradually increase request rate, measure drop ratio
    // Start at 5 rps, increase to 10, 20, 50, 100, 200
    // Stop when drop ratio > 20%

    let rates = [5, 10, 20, 50, 100, 200, 500];
    let mut max_good_rate = 1;

    for &rate in &rates {
        let (sent, received) = test_rate(ip, doh_url, domain, rate, 2).await; // 2 sec test
        let success_pct = (received as f64 / sent as f64) * 100.0;

        eprintln!("    {} rps: {}/{} ({:.0}%)", rate, received, sent, success_pct);

        if success_pct >= 80.0 {
            max_good_rate = rate;
        } else {
            break; // hit the limit
        }
    }

    max_good_rate
}

async fn test_rate(ip: &str, doh_url: Option<&str>, domain: &str, target_rps: u32, duration_secs: u32) -> (u32, u32) {
    let interval = Duration::from_micros(1_000_000 / target_rps as u64);
    let total_queries = target_rps * duration_secs;
    let mut sent = 0u32;
    let mut received = 0u32;

    // Use tokio tasks: sender sends at target rate, results collected
    let (tx, mut rx) = tokio::sync::mpsc::channel(total_queries as usize);

    // Sender
    let domain_clone = domain.to_string();
    let ip_clone = ip.to_string();
    let doh_clone = doh_url.map(|s| s.to_string());
    let sender = tokio::spawn(async move {
        let mut count = 0u32;
        let start = Instant::now();
        while count < total_queries {
            let d = domain_clone.clone();
            let i = ip_clone.clone();
            let doh = doh_clone.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let probe = prober::probe_resolver(&i, doh.as_deref(), &d).await;
                let _ = tx.send(probe.alive).await;
            });
            count += 1;
            // Pace to target rate
            let expected = Duration::from_micros(count as u64 * 1_000_000 / target_rps as u64);
            let elapsed = start.elapsed();
            if expected > elapsed {
                tokio::time::sleep(expected - elapsed).await;
            }
        }
        count
    });

    sent = sender.await.unwrap_or(0);

    // Collect results (with timeout)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while let Ok(result) = tokio::time::timeout_at(deadline, rx.recv()).await {
        match result {
            Some(true) => received += 1,
            Some(false) => {} // timeout/error
            None => break,    // channel closed
        }
    }

    (sent, received)
}
```

#### Resolver fingerprinting:
```rust
async fn fingerprint_resolver(ip: &str) -> Option<String> {
    // Method 1: Query version.bind CHAOS TXT (BIND reports version)
    // Method 2: Query hostname.bind CHAOS TXT
    // Method 3: Check NSID in EDNS0 response
    // Method 4: Analyze response header patterns

    // Try version.bind
    if let Ok(version) = query_chaos_txt(ip, "version.bind").await {
        if version.to_lowercase().contains("bind") { return Some(format!("BIND ({})", version)); }
        if version.to_lowercase().contains("unbound") { return Some(format!("Unbound ({})", version)); }
        if version.to_lowercase().contains("powerdns") { return Some(format!("PowerDNS ({})", version)); }
        if version.to_lowercase().contains("knot") { return Some(format!("Knot ({})", version)); }
        return Some(format!("Unknown ({})", version));
    }

    // Try hostname.bind
    if let Ok(hostname) = query_chaos_txt(ip, "hostname.bind").await {
        // Can identify cloud providers by hostname
        if hostname.contains("google") { return Some("Google Public DNS".to_string()); }
        if hostname.contains("cloudflare") { return Some("Cloudflare".to_string()); }
        return Some(format!("Unknown (host: {})", hostname));
    }

    None
}

async fn query_chaos_txt(ip: &str, name: &str) -> Result<String> {
    // Build DNS CHAOS TXT query
    use hickory_proto::op::{Message, MessageType, OpCode, Query};
    use hickory_proto::rr::{DNSClass, Name, RecordType, RData};
    use hickory_proto::serialize::binary::{BinEncodable, BinDecodable};

    let qname: Name = name.parse()?;
    let mut message = Message::new();
    message.set_id(rand::random::<u16>());
    message.set_message_type(MessageType::Query);
    message.set_op_code(OpCode::Query);
    message.set_recursion_desired(false); // CHAOS queries are not recursive

    let mut query = Query::new();
    query.set_name(qname);
    query.set_query_type(RecordType::TXT);
    query.set_query_class(DNSClass::CH); // CHAOS class
    message.add_query(query);

    let wire = message.to_bytes()?;

    // Send via UDP
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(format!("{}:53", ip)).await?;
    socket.send(&wire).await?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await??;

    let resp = Message::from_bytes(&buf[..n])?;
    for answer in resp.answers() {
        if let RData::TXT(txt) = answer.data() {
            let text: String = txt.txt_data().iter()
                .map(|s| String::from_utf8_lossy(s).to_string())
                .collect::<Vec<_>>()
                .join("");
            return Ok(text);
        }
    }

    anyhow::bail!("no TXT in response")
}
```

#### Forwarder detection:
```rust
async fn detect_forwarder(ip: &str, domain: &str) -> (bool, Vec<String>) {
    // Send 5 probe queries, collect real resolver IPs
    // If real resolver IP != ip → this is a forwarder
    let mut real_ips: std::collections::HashSet<String> = std::collections::HashSet::new();

    for _ in 0..5 {
        let deleg = prober::test_delegation(ip, None, domain).await;
        if let Some(real_ip) = deleg.real_resolver_ip {
            real_ips.insert(real_ip);
        }
    }

    let is_forwarder = real_ips.iter().any(|r| r != ip);
    let upstreams: Vec<String> = real_ips.into_iter().filter(|r| r != ip).collect();

    (is_forwarder, upstreams)
}
```

#### Honeypot detection:
```rust
async fn check_honeypot(ip: &str) -> u8 {
    let mut score: u8 = 0;

    // 1. Query non-existent domain — should get NXDOMAIN
    let nxdomain_test = query_a_record(ip, "thisdomain-definitely-does-not-exist-xyz123.com").await;
    match nxdomain_test {
        Ok(Some(_ip)) => score += 40, // resolved non-existent domain = suspicious
        Ok(None) => {}                // NXDOMAIN = expected
        Err(_) => {}                  // error = ok
    }

    // 2. Check reverse DNS — security companies often have identifiable rDNS
    let rdns = query_ptr(ip).await;
    if let Some(name) = rdns {
        let suspicious_words = ["honeypot", "sinkhole", "security", "malware", "threat", "sandbox"];
        for word in &suspicious_words {
            if name.to_lowercase().contains(word) {
                score += 30;
                break;
            }
        }
    }

    // 3. Too-perfect behavior is suspicious
    // (would need multiple tests over time — skip for now, just note it)

    score.min(100)
}

async fn query_a_record(resolver_ip: &str, domain: &str) -> Result<Option<String>> {
    let query = sp_transport::build_dns_query(domain)?;
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(format!("{}:53", resolver_ip)).await?;
    socket.send(&query.wire).await?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await??;

    use hickory_proto::op::Message;
    use hickory_proto::rr::RData;
    use hickory_proto::serialize::binary::BinDecodable;
    let msg = Message::from_bytes(&buf[..n])?;

    for answer in msg.answers() {
        if let RData::A(a) = answer.data() {
            return Ok(Some(a.0.to_string()));
        }
    }
    Ok(None) // NXDOMAIN or no A records
}

async fn query_ptr(ip: &str) -> Option<String> {
    // Reverse DNS lookup using system resolver
    use tokio::net::lookup_host;
    // This is simplified — proper PTR would query in-addr.arpa
    // For now just try reverse lookup
    let addr = format!("{}:0", ip);
    // tokio doesn't have direct PTR, use DNS query
    // Simplified: skip for now, return None
    None
}
```

#### Response manipulation check:
```rust
async fn check_response_manipulation(ip: &str) -> bool {
    // Query well-known domains and compare with expected answers
    // example.com A should be 93.184.215.14
    let result = query_a_record(ip, "example.com").await;
    match result {
        Ok(Some(actual_ip)) => actual_ip != "93.184.215.14",
        _ => false, // couldn't test
    }
}
```

#### Main profile function:
```rust
pub async fn run_profile(config: ProfileConfig) -> Result<()> {
    // Load resolvers from input file
    let ips = load_resolver_list(&config.input)?;
    let total = ips.len();
    eprintln!("Profiling {} resolvers...\n", total);

    let mut profiles: Vec<ResolverProfile> = Vec::new();

    for (i, ip) in ips.iter().enumerate() {
        eprintln!("[{}/{}] Profiling {}...", i + 1, total, ip);

        // Rate limit (takes ~15 sec per resolver)
        eprint!("  Rate limit: ");
        let rate_limit = detect_rate_limit(ip, None, &config.domain).await;
        eprintln!("  → max {} rps", rate_limit);

        // Latency (10 probes)
        eprint!("  Latency: ");
        let (avg, min, max) = measure_latency(ip, None, &config.domain, 10).await;
        eprintln!("avg={}ms min={}ms max={}ms jitter={}ms", avg, min, max, max - min);

        // Fingerprint
        eprint!("  Fingerprint: ");
        let software = fingerprint_resolver(ip).await;
        eprintln!("{}", software.as_deref().unwrap_or("unknown"));

        // Forwarder
        eprint!("  Forwarder: ");
        let (is_fwd, upstreams) = detect_forwarder(ip, &config.domain).await;
        if is_fwd {
            eprintln!("YES → {}", upstreams.join(", "));
        } else {
            eprintln!("NO (direct recursive)");
        }

        // Honeypot
        eprint!("  Honeypot check: ");
        let hp_score = check_honeypot(ip).await;
        if hp_score > 30 {
            eprintln!("SUSPICIOUS (score {})", hp_score);
        } else {
            eprintln!("OK (score {})", hp_score);
        }

        // Response manipulation
        eprint!("  Manipulation: ");
        let manipulated = check_response_manipulation(ip).await;
        eprintln!("{}", if manipulated { "YES — responses modified!" } else { "NO" });

        profiles.push(ResolverProfile {
            ip: ip.clone(),
            name: ip.clone(),
            doh_url: None,
            rate_limit_rps: rate_limit,
            rate_limit_method: "gradual-increase".to_string(),
            latency_avg_ms: avg,
            latency_min_ms: min,
            latency_max_ms: max,
            latency_jitter_ms: max - min,
            edns0_max: 4096, // from validate
            supported_types: vec![], // from validate
            best_type: "TXT".to_string(),
            software,
            is_forwarder: is_fwd,
            upstream_resolvers: upstreams,
            delegation_works: true,
            response_manipulation: manipulated,
            dnssec_validates: false, // TODO
            honeypot_score: hp_score,
        });

        eprintln!();
    }

    // Sort by rate_limit descending (best first)
    profiles.sort_by(|a, b| b.rate_limit_rps.cmp(&a.rate_limit_rps));

    // Print summary
    eprintln!("=== Profile Summary ===");
    eprintln!("{:<20} {:>8} {:>8} {:>10} {:>10} {:>8}", "Resolver", "RPS", "Latency", "Software", "Forwarder", "Honeypot");
    eprintln!("{}", "-".repeat(74));
    for p in &profiles {
        eprintln!("{:<20} {:>8} {:>6}ms {:>10} {:>10} {:>8}",
            p.ip, p.rate_limit_rps, p.latency_avg_ms,
            p.software.as_deref().unwrap_or("?"),
            if p.is_forwarder { "yes" } else { "no" },
            p.honeypot_score);
    }

    // Write output
    if let Some(output) = &config.output {
        write_profile_toml(&profiles, output)?;
        eprintln!("\nWritten to {}", output);
    }

    Ok(())
}

fn load_resolver_list(path: &str) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    // Support both alive.txt (one IP per line) and TOML (parse ip fields)
    if path.ends_with(".toml") {
        // Parse TOML, extract IPs
        // ... toml parsing for [[resolver]] ip = ["x.x.x.x"]
        let config: toml::Value = toml::from_str(&content)?;
        let mut ips = Vec::new();
        if let Some(resolvers) = config.get("resolver").and_then(|v| v.as_array()) {
            for r in resolvers {
                if let Some(ip_arr) = r.get("ip").and_then(|v| v.as_array()) {
                    for ip in ip_arr {
                        if let Some(s) = ip.as_str() {
                            ips.push(s.to_string());
                        }
                    }
                }
            }
        }
        Ok(ips)
    } else {
        // Plain text, one IP per line
        Ok(content.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect())
    }
}

async fn measure_latency(ip: &str, doh_url: Option<&str>, domain: &str, probes: u32) -> (u64, u64, u64) {
    let mut latencies = Vec::new();
    for _ in 0..probes {
        let result = prober::probe_resolver(ip, doh_url, domain).await;
        if let Some(ms) = result.latency_ms {
            latencies.push(ms);
        }
    }
    if latencies.is_empty() {
        return (0, 0, 0);
    }
    let avg = latencies.iter().sum::<u64>() / latencies.len() as u64;
    let min = *latencies.iter().min().unwrap();
    let max = *latencies.iter().max().unwrap();
    (avg, min, max)
}

fn write_profile_toml(profiles: &[ResolverProfile], path: &str) -> Result<()> {
    let mut content = String::from("# Profiled DNS resolvers\n\n");
    for p in profiles {
        content.push_str("[[resolver]]\n");
        content.push_str(&format!("name = \"{}\"\n", p.name));
        content.push_str(&format!("ip = [\"{}\"]\n", p.ip));
        if let Some(ref doh) = p.doh_url {
            content.push_str(&format!("doh = \"{}\"\n", doh));
        }
        content.push_str(&format!("rate_limit_rps = {}\n", p.rate_limit_rps));
        content.push_str(&format!("latency_avg_ms = {}\n", p.latency_avg_ms));
        content.push_str(&format!("latency_min_ms = {}\n", p.latency_min_ms));
        content.push_str(&format!("latency_max_ms = {}\n", p.latency_max_ms));
        if let Some(ref sw) = p.software {
            content.push_str(&format!("software = \"{}\"\n", sw));
        }
        content.push_str(&format!("is_forwarder = {}\n", p.is_forwarder));
        if !p.upstream_resolvers.is_empty() {
            let ups: Vec<String> = p.upstream_resolvers.iter().map(|s| format!("\"{}\"", s)).collect();
            content.push_str(&format!("upstream_resolvers = [{}]\n", ups.join(", ")));
        }
        content.push_str(&format!("response_manipulation = {}\n", p.response_manipulation));
        content.push_str(&format!("honeypot_score = {}\n", p.honeypot_score));
        content.push('\n');
    }
    std::fs::write(path, content)?;
    Ok(())
}
```

### 3. Monitor command (monitor.rs)

`dns-scanner monitor` — continuous monitoring of resolvers.

```rust
pub struct MonitorConfig {
    pub input: String,
    pub domain: String,
    pub interval_secs: u64,  // default 600 (10 min)
}

pub async fn run_monitor(config: MonitorConfig) -> Result<()> {
    let ips = load_resolver_list(&config.input)?;
    eprintln!("Monitoring {} resolvers every {} seconds", ips.len(), config.interval_secs);
    eprintln!("Press Ctrl+C to stop\n");

    let mut history: HashMap<String, Vec<CheckResult>> = HashMap::new();

    loop {
        let now = chrono::Local::now(); // add chrono dep, or use std SystemTime
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_secs();

        eprintln!("=== Check at {} ===", timestamp);

        for ip in &ips {
            let probe = prober::probe_resolver(ip, None, &config.domain).await;
            let deleg = if probe.alive {
                let d = prober::test_delegation(ip, None, &config.domain).await;
                d.works
            } else {
                false
            };

            let result = CheckResult {
                timestamp,
                alive: probe.alive,
                delegation: deleg,
                latency_ms: probe.latency_ms,
            };

            let entry = history.entry(ip.clone()).or_insert_with(Vec::new);
            entry.push(result.clone());

            // Calculate uptime
            let total = entry.len();
            let alive_count = entry.iter().filter(|r| r.alive).count();
            let deleg_count = entry.iter().filter(|r| r.delegation).count();
            let uptime_pct = (alive_count as f64 / total as f64) * 100.0;
            let deleg_pct = (deleg_count as f64 / total as f64) * 100.0;

            let status = if deleg { "OK" } else if probe.alive { "NO-DELEG" } else { "DOWN" };
            let latency_str = probe.latency_ms.map(|ms| format!("{}ms", ms)).unwrap_or("---".to_string());

            eprintln!("  {} {:>7} {:>6}  uptime={:.0}% deleg={:.0}% ({} checks)",
                ip, status, latency_str, uptime_pct, deleg_pct, total);

            // Alert on state change
            if total >= 2 {
                let prev = &entry[total - 2];
                if prev.delegation && !deleg {
                    eprintln!("  *** ALERT: {} delegation LOST! ***", ip);
                }
                if !prev.alive && probe.alive {
                    eprintln!("  *** ALERT: {} came BACK UP ***", ip);
                }
            }
        }

        eprintln!();
        tokio::time::sleep(Duration::from_secs(config.interval_secs)).await;
    }
}

#[derive(Debug, Clone)]
struct CheckResult {
    timestamp: u64,
    alive: bool,
    delegation: bool,
    latency_ms: Option<u64>,
}
```

### 4. Update CLI (main.rs)

Update Profile command:
```rust
Profile {
    /// Input resolver list (alive.txt or resolvers.toml)
    #[arg(default_value = "alive.txt")]
    input: String,

    /// Tunnel domain
    #[arg(long, default_value = "tunnel.example.com")]
    domain: String,
},
```

Update Monitor command:
```rust
Monitor {
    /// Input resolver list
    #[arg(default_value = "alive.txt")]
    input: String,

    /// Check interval in seconds
    #[arg(long, default_value = "600")]
    interval: u64,

    /// Tunnel domain
    #[arg(long, default_value = "tunnel.example.com")]
    domain: String,
},
```

Update match arms to pass config structs to profile::run_profile and monitor::run_monitor.

Also update Scan match arm to handle "auto" mode string:
```rust
Commands::Scan { mode } => {
    let scan_mode = match mode.as_str() {
        "aggressive" => scanner::ScanMode::Aggressive,
        "stealth" => scanner::ScanMode::Stealth,
        _ => scanner::ScanMode::Auto, // "auto" or anything else
    };
    // ...
}
```

### 5. Build and test

1. `cargo build` — zero errors
2. `cargo test -p sp-scanner` — all tests pass
3. Test:
   - `dns-scanner scan --domain tunnel.example.com` (auto mode, should detect and choose)
   - `dns-scanner scan --mode stealth --domain tunnel.example.com`
   - `dns-scanner scan --mode aggressive --domain tunnel.example.com`
   - `dns-scanner profile /tmp/alive-test.txt --domain tunnel.example.com` (test with alive.txt from previous test; this takes time — try with just 3-5 resolvers: create a small test file with 3 IPs like 8.8.8.8, 1.1.1.1, 9.9.9.9)
   - `dns-scanner monitor /tmp/alive-test.txt --interval 10 --domain tunnel.example.com` (test with 10s interval, Ctrl+C after 2 cycles)
4. Fix any warnings

## Constraints
- Reuse prober.rs functions
- Rate limit test is slow (~15s per resolver per rate level) — that's ok for profile
- Monitor runs forever until Ctrl+C — that's the intended behavior
- Don't add chrono dependency — use std::time::SystemTime for timestamps
- Profile should support both alive.txt (plain IPs) and TOML input
- Commit when done

## File changes
```
MODIFY: crates/sp-scanner/src/main.rs       ← update CLI for auto mode, profile args, monitor args
MODIFY: crates/sp-scanner/src/scanner.rs     ← add Auto mode, auto_detect_mode()
REPLACE: crates/sp-scanner/src/profile.rs    ← full implementation
REPLACE: crates/sp-scanner/src/monitor.rs    ← full implementation
```

// Phase 3: Deep resolver analysis — rate limits, fingerprinting, honeypot detection.

use anyhow::Result;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

use crate::prober;

pub struct ProfileConfig {
    pub input: String,
    pub domain: String,
    pub output: Option<String>,
}

#[allow(dead_code)]
pub struct ResolverProfile {
    pub ip: String,
    pub name: String,
    pub doh_url: Option<String>,
    pub rate_limit_rps: u32,
    pub rate_limit_method: String,
    pub latency_avg_ms: u64,
    pub latency_min_ms: u64,
    pub latency_max_ms: u64,
    pub latency_jitter_ms: u64,
    pub software: Option<String>,
    pub is_forwarder: bool,
    pub upstream_resolvers: Vec<String>,
    pub response_manipulation: bool,
    pub honeypot_score: u8,
    pub cache_ttl_secs: Option<u64>,
    pub dnssec_validates: bool,
    pub tcp_supported: bool,
}

pub async fn run_profile(config: ProfileConfig) -> Result<()> {
    let ips = load_resolver_list(&config.input)?;
    let total = ips.len();
    if total == 0 {
        anyhow::bail!("no resolvers found in {}", config.input);
    }
    eprintln!("Profiling {} resolvers...\n", total);

    let mut profiles: Vec<ResolverProfile> = Vec::new();

    for (i, ip) in ips.iter().enumerate() {
        eprintln!("[{}/{}] Profiling {}...", i + 1, total, ip);

        // Rate limit
        eprint!("  Rate limit: ");
        let rate_limit = detect_rate_limit(ip, None, &config.domain).await;
        eprintln!("  → max {} rps", rate_limit);

        // Latency
        eprint!("  Latency: ");
        let (avg, min, max) = measure_latency(ip, None, &config.domain, 10).await;
        let jitter = max.saturating_sub(min);
        eprintln!(
            "avg={}ms min={}ms max={}ms jitter={}ms",
            avg, min, max, jitter
        );

        // Fingerprint
        eprint!("  Fingerprint: ");
        let software = fingerprint_resolver(ip).await;
        eprintln!("{}", software.as_deref().unwrap_or("unknown"));

        // Forwarder detection
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
        eprintln!(
            "{}",
            if manipulated {
                "YES — responses modified!"
            } else {
                "NO"
            }
        );

        // Cache TTL detection
        eprint!("  Cache TTL: ");
        let cache_ttl = detect_cache_ttl(ip, &config.domain).await;
        match cache_ttl {
            Some(ttl) => eprintln!("{}s", ttl),
            None => eprintln!("unknown"),
        }

        // DNSSEC validation check
        eprint!("  DNSSEC: ");
        let dnssec = check_dnssec_validation(ip).await;
        eprintln!("{}", if dnssec { "validates" } else { "does NOT validate" });

        // TCP support check
        eprint!("  TCP: ");
        let tcp = check_tcp_support(ip, &config.domain).await;
        eprintln!("{}", if tcp { "supported" } else { "NOT supported" });

        profiles.push(ResolverProfile {
            ip: ip.clone(),
            name: ip.clone(),
            doh_url: None,
            rate_limit_rps: rate_limit,
            rate_limit_method: "gradual-increase".to_string(),
            latency_avg_ms: avg,
            latency_min_ms: min,
            latency_max_ms: max,
            latency_jitter_ms: jitter,
            software,
            is_forwarder: is_fwd,
            upstream_resolvers: upstreams,
            response_manipulation: manipulated,
            honeypot_score: hp_score,
            cache_ttl_secs: cache_ttl,
            dnssec_validates: dnssec,
            tcp_supported: tcp,
        });

        eprintln!();
    }

    // Sort by rate_limit descending (best first)
    profiles.sort_by(|a, b| b.rate_limit_rps.cmp(&a.rate_limit_rps));

    // Print summary
    eprintln!("=== Profile Summary ===");
    eprintln!(
        "{:<20} {:>8} {:>8} {:>12} {:>10} {:>8} {:>8} {:>6} {:>4}",
        "Resolver", "RPS", "Latency", "Software", "Forwarder", "Honeypot", "CacheTTL", "DNSSEC", "TCP"
    );
    eprintln!("{}", "-".repeat(104));
    for p in &profiles {
        let ttl_str = p
            .cache_ttl_secs
            .map(|t| format!("{}s", t))
            .unwrap_or_else(|| "?".to_string());
        eprintln!(
            "{:<20} {:>8} {:>6}ms {:>12} {:>10} {:>8} {:>8} {:>6} {:>4}",
            p.ip,
            p.rate_limit_rps,
            p.latency_avg_ms,
            p.software.as_deref().unwrap_or("?"),
            if p.is_forwarder { "yes" } else { "no" },
            p.honeypot_score,
            ttl_str,
            if p.dnssec_validates { "yes" } else { "no" },
            if p.tcp_supported { "yes" } else { "no" },
        );
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
    if path.ends_with(".toml") {
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
        Ok(content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect())
    }
}

async fn detect_rate_limit(ip: &str, doh_url: Option<&str>, domain: &str) -> u32 {
    let rates = [5, 10, 20, 50, 100, 200, 500];
    let mut max_good_rate = 1;

    for &rate in &rates {
        let (sent, received) = test_rate(ip, doh_url, domain, rate, 2).await;
        if sent == 0 {
            break;
        }
        let success_pct = (received as f64 / sent as f64) * 100.0;
        eprintln!(
            "    {} rps: {}/{} ({:.0}%)",
            rate, received, sent, success_pct
        );

        if success_pct >= 80.0 {
            max_good_rate = rate;
        } else {
            break;
        }
    }

    max_good_rate
}

async fn test_rate(
    ip: &str,
    doh_url: Option<&str>,
    domain: &str,
    target_rps: u32,
    duration_secs: u32,
) -> (u32, u32) {
    let total_queries = target_rps * duration_secs;
    let (tx, mut rx) = tokio::sync::mpsc::channel(total_queries as usize + 1);

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
            let expected =
                Duration::from_micros(count as u64 * 1_000_000 / target_rps as u64);
            let elapsed = start.elapsed();
            if expected > elapsed {
                tokio::time::sleep(expected - elapsed).await;
            }
        }
        count
    });

    let sent = sender.await.unwrap_or(0);

    // Collect results with timeout
    let mut received = 0u32;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while let Ok(result) = tokio::time::timeout_at(deadline, rx.recv()).await {
        match result {
            Some(true) => received += 1,
            Some(false) => {}
            None => break,
        }
    }

    (sent, received)
}

async fn measure_latency(
    ip: &str,
    doh_url: Option<&str>,
    domain: &str,
    probes: u32,
) -> (u64, u64, u64) {
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

async fn fingerprint_resolver(ip: &str) -> Option<String> {
    // Method 1: Query version.bind CHAOS TXT
    if let Ok(version) = query_chaos_txt(ip, "version.bind").await {
        let lower = version.to_lowercase();
        if lower.contains("bind") {
            return Some(format!("BIND ({})", version));
        }
        if lower.contains("unbound") {
            return Some(format!("Unbound ({})", version));
        }
        if lower.contains("powerdns") {
            return Some(format!("PowerDNS ({})", version));
        }
        if lower.contains("knot") {
            return Some(format!("Knot ({})", version));
        }
        return Some(format!("Unknown ({})", version));
    }

    // Method 2: Query hostname.bind CHAOS TXT
    if let Ok(hostname) = query_chaos_txt(ip, "hostname.bind").await {
        if hostname.contains("google") {
            return Some("Google Public DNS".to_string());
        }
        if hostname.contains("cloudflare") {
            return Some("Cloudflare".to_string());
        }
        return Some(format!("Unknown (host: {})", hostname));
    }

    None
}

async fn query_chaos_txt(ip: &str, name: &str) -> Result<String> {
    use hickory_proto::op::{Message, MessageType, OpCode, Query};
    use hickory_proto::rr::{DNSClass, Name, RData, RecordType};
    use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};

    let qname: Name = name.parse()?;
    let mut message = Message::new();
    message.set_id(rand::random::<u16>());
    message.set_message_type(MessageType::Query);
    message.set_op_code(OpCode::Query);
    message.set_recursion_desired(false);

    let mut query = Query::new();
    query.set_name(qname);
    query.set_query_type(RecordType::TXT);
    query.set_query_class(DNSClass::CH);
    message.add_query(query);

    let wire = message
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("encode: {}", e))?;

    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(format!("{}:53", ip)).await?;
    socket.send(&wire).await?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("timeout"))??;

    let resp = Message::from_bytes(&buf[..n])?;
    for answer in resp.answers() {
        if let RData::TXT(txt) = answer.data() {
            let text: String = txt
                .txt_data()
                .iter()
                .map(|s| String::from_utf8_lossy(s).to_string())
                .collect::<Vec<_>>()
                .join("");
            return Ok(text);
        }
    }

    anyhow::bail!("no TXT in response")
}

async fn detect_forwarder(ip: &str, domain: &str) -> (bool, Vec<String>) {
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

async fn check_honeypot(ip: &str) -> u8 {
    let mut score: u8 = 0;

    // Query non-existent domain — should get NXDOMAIN
    let nxdomain_test =
        query_a_record(ip, "thisdomain-definitely-does-not-exist-xyz123.com").await;
    match nxdomain_test {
        Ok(Some(_)) => score += 40, // resolved non-existent domain = suspicious
        Ok(None) => {}              // NXDOMAIN = expected
        Err(_) => {}                // error = ok
    }

    // Check reverse DNS for suspicious keywords
    if let Some(name) = query_ptr(ip).await {
        let suspicious = [
            "honeypot", "sinkhole", "security", "malware", "threat", "sandbox",
        ];
        for word in &suspicious {
            if name.to_lowercase().contains(word) {
                score += 30;
                break;
            }
        }
    }

    score.min(100)
}

async fn query_a_record(resolver_ip: &str, domain: &str) -> Result<Option<String>> {
    let query = sp_transport::build_dns_query(domain)?;
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(format!("{}:53", resolver_ip)).await?;
    socket.send(&query.wire).await?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("timeout"))??;

    use hickory_proto::op::Message;
    use hickory_proto::rr::RData;
    use hickory_proto::serialize::binary::BinDecodable;
    let msg = Message::from_bytes(&buf[..n])?;

    for answer in msg.answers() {
        if let RData::A(a) = answer.data() {
            return Ok(Some(a.0.to_string()));
        }
    }
    Ok(None)
}

async fn query_ptr(ip: &str) -> Option<String> {
    // Build in-addr.arpa name from IP
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let arpa = format!(
        "{}.{}.{}.{}.in-addr.arpa",
        parts[3], parts[2], parts[1], parts[0]
    );

    use hickory_proto::op::{Message, MessageType, OpCode, Query};
    use hickory_proto::rr::{Name, RData, RecordType};
    use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};

    let qname: Name = arpa.parse().ok()?;
    let mut message = Message::new();
    message.set_id(rand::random::<u16>());
    message.set_message_type(MessageType::Query);
    message.set_op_code(OpCode::Query);
    message.set_recursion_desired(true);

    let mut query = Query::new();
    query.set_name(qname);
    query.set_query_type(RecordType::PTR);
    message.add_query(query);

    let wire = message.to_bytes().ok()?;

    // Use the resolver itself for the PTR lookup
    let socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    socket.connect(format!("{}:53", ip)).await.ok()?;
    socket.send(&wire).await.ok()?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf))
        .await
        .ok()?
        .ok()?;

    let resp = Message::from_bytes(&buf[..n]).ok()?;
    for answer in resp.answers() {
        if let RData::PTR(ptr) = answer.data() {
            return Some(ptr.0.to_string());
        }
    }

    None
}

async fn check_response_manipulation(ip: &str) -> bool {
    // Query example.com A and compare with expected answer
    let result = query_a_record(ip, "example.com").await;
    match result {
        Ok(Some(actual_ip)) => actual_ip != "93.184.215.14",
        _ => false,
    }
}

async fn detect_cache_ttl(ip: &str, domain: &str) -> Option<u64> {
    let unique = format!("cachettl-{:08x}.{}", rand::random::<u32>(), domain);

    // First query (uncached)
    let q1 = sp_transport::build_dns_query(&unique).ok()?;
    let socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    socket.connect(format!("{}:53", ip)).await.ok()?;
    socket.send(&q1.wire).await.ok()?;
    let mut buf = vec![0u8; 4096];
    let _ = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf))
        .await
        .ok()?
        .ok()?;

    // Wait 2 seconds
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Second query (should be cached)
    socket.send(&q1.wire).await.ok()?;
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf))
        .await
        .ok()?
        .ok()?;

    // Parse TTL from response
    use hickory_proto::op::Message;
    use hickory_proto::serialize::binary::BinDecodable;
    let msg = Message::from_bytes(&buf[..n]).ok()?;
    let ttl = msg.answers().first().map(|a| a.ttl());

    ttl.map(|t| t as u64)
}

async fn check_dnssec_validation(ip: &str) -> bool {
    // dnssec-failed.org has intentionally broken DNSSEC.
    // Validating resolvers return SERVFAIL; non-validating ones return the A record.
    let query = match sp_transport::build_dns_query("dnssec-failed.org") {
        Ok(q) => q,
        Err(_) => return false,
    };

    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(_) => return false,
    };

    if socket.connect(format!("{}:53", ip)).await.is_err() {
        return false;
    }
    if socket.send(&query.wire).await.is_err() {
        return false;
    }

    let mut buf = vec![0u8; 4096];
    match tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await {
        Ok(Ok(n)) => {
            use hickory_proto::op::Message;
            use hickory_proto::serialize::binary::BinDecodable;
            if let Ok(msg) = Message::from_bytes(&buf[..n]) {
                // SERVFAIL = resolver validates DNSSEC (correctly rejected broken record)
                msg.response_code() == hickory_proto::op::ResponseCode::ServFail
            } else {
                false
            }
        }
        _ => false,
    }
}

async fn check_tcp_support(ip: &str, domain: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr: std::net::SocketAddr = match format!("{}:53", ip).parse() {
        Ok(a) => a,
        Err(_) => return false,
    };

    let mut tcp = match tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    {
        Ok(Ok(s)) => s,
        _ => return false,
    };

    let query = match sp_transport::build_dns_query(&format!("tcp-test.{}", domain)) {
        Ok(q) => q,
        Err(_) => return false,
    };

    let wire = query.wire.to_vec();
    let len = (wire.len() as u16).to_be_bytes();

    if tcp.write_all(&len).await.is_err() {
        return false;
    }
    if tcp.write_all(&wire).await.is_err() {
        return false;
    }

    let mut len_buf = [0u8; 2];
    match tokio::time::timeout(Duration::from_secs(3), tcp.read_exact(&mut len_buf)).await {
        Ok(Ok(_)) => {
            let resp_len = u16::from_be_bytes(len_buf) as usize;
            if resp_len > 65535 {
                return false;
            }
            let mut resp_buf = vec![0u8; resp_len];
            matches!(
                tokio::time::timeout(Duration::from_secs(3), tcp.read_exact(&mut resp_buf)).await,
                Ok(Ok(_))
            )
        }
        _ => false,
    }
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
            let ups: Vec<String> =
                p.upstream_resolvers.iter().map(|s| format!("\"{}\"", s)).collect();
            content.push_str(&format!("upstream_resolvers = [{}]\n", ups.join(", ")));
        }
        content.push_str(&format!(
            "response_manipulation = {}\n",
            p.response_manipulation
        ));
        content.push_str(&format!("honeypot_score = {}\n", p.honeypot_score));
        if let Some(ttl) = p.cache_ttl_secs {
            content.push_str(&format!("cache_ttl_secs = {}\n", ttl));
        }
        content.push_str(&format!("dnssec_validates = {}\n", p.dnssec_validates));
        content.push_str(&format!("tcp_supported = {}\n", p.tcp_supported));
        content.push('\n');
    }
    std::fs::write(path, content)?;
    Ok(())
}

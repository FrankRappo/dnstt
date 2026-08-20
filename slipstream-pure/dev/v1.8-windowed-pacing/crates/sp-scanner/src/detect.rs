// Phase 2: Network environment detection.
// Analyzes the local network to discover DNS configuration, transparent proxies,
// hijacking, port reachability, and captive portals.

use anyhow::Result;
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::debug;

use crate::prober;

/// Full network analysis report.
#[derive(Debug, Clone)]
pub struct NetworkReport {
    pub system_dns: Vec<String>,
    pub real_resolvers: Vec<String>,
    pub transparent_proxy: bool,
    pub dns_hijack: bool,
    pub port53_udp: bool,
    pub port53_tcp: bool,
    pub port443_https: bool,
    pub port853_tls: bool,
    pub captive_portal: bool,
}

/// Run the detect command: analyze network and print report.
pub async fn run_detect(domain: &str) -> Result<()> {
    let report = analyze_network(domain).await;
    print_report(&report);
    Ok(())
}

/// Full network analysis — reusable by bootstrap.
pub async fn analyze_network(domain: &str) -> NetworkReport {
    println!("=== Network Analysis ===\n");

    // Step 1: System DNS
    println!("[1/6] Reading system DNS config...");
    let system_dns = read_system_dns();
    for ip in &system_dns {
        println!("  Found: {}", ip);
    }

    // Step 2: DNS leak — discover real resolvers
    println!("[2/6] Discovering real resolvers (DNS leak)...");
    let real_resolvers = discover_real_resolvers(&system_dns, domain).await;
    for ip in &real_resolvers {
        println!("  Discovered: {}", ip);
    }

    // Step 3: Transparent proxy
    println!("[3/6] Testing for transparent DNS proxy...");
    let transparent_proxy = test_transparent_proxy(domain).await;
    println!("  Transparent proxy: {}", if transparent_proxy { "YES" } else { "NO" });

    // Step 4: DNS hijack
    println!("[4/6] Testing for DNS hijacking...");
    let dns_hijack = test_dns_hijack(&system_dns).await;
    println!("  DNS hijacking: {}", if dns_hijack { "YES" } else { "NO" });

    // Step 5: Port reachability (parallel)
    println!("[5/6] Testing port reachability...");
    let (udp53, tcp53, https443, tls853) = test_port_reachability().await;
    println!("  UDP 53:    {}", if udp53 { "OPEN" } else { "BLOCKED" });
    println!("  TCP 53:    {}", if tcp53 { "OPEN" } else { "BLOCKED" });
    println!("  HTTPS 443: {}", if https443 { "OPEN" } else { "BLOCKED" });
    println!("  DoT 853:   {}", if tls853 { "OPEN" } else { "BLOCKED" });

    // Step 6: Captive portal
    println!("[6/6] Testing for captive portal...");
    let captive_portal = test_captive_portal().await;
    println!("  Captive portal: {}", if captive_portal { "YES" } else { "NO" });

    NetworkReport {
        system_dns,
        real_resolvers,
        transparent_proxy,
        dns_hijack,
        port53_udp: udp53,
        port53_tcp: tcp53,
        port443_https: https443,
        port853_tls: tls853,
        captive_portal,
    }
}

/// Read system DNS configuration from /etc/resolv.conf (Linux).
pub fn read_system_dns() -> Vec<String> {
    let mut dns_servers = Vec::new();

    // Parse /etc/resolv.conf
    if let Ok(contents) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in contents.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("nameserver") {
                let ip = rest.trim();
                if !ip.is_empty() && !dns_servers.contains(&ip.to_string()) {
                    dns_servers.push(ip.to_string());
                }
            }
        }
    }

    // If nothing found, try common gateway IPs
    if dns_servers.is_empty() {
        debug!("No nameservers in resolv.conf, trying common gateways");
        dns_servers.push("192.168.1.1".to_string());
        dns_servers.push("192.168.0.1".to_string());
        dns_servers.push("10.0.0.1".to_string());
    }

    dns_servers
}

/// Discover real resolvers via DNS leak technique.
/// Sends probe queries through system DNS and reads back the real resolver IP.
async fn discover_real_resolvers(system_dns: &[String], domain: &str) -> Vec<String> {
    let mut discovered = std::collections::HashSet::new();

    for dns_ip in system_dns {
        // Send 5 probes to discover resolver pool
        for _ in 0..5 {
            let deleg = prober::test_delegation(dns_ip, None, domain).await;
            if let Some(real_ip) = deleg.real_resolver_ip {
                discovered.insert(real_ip);
            }
        }
    }

    discovered.into_iter().collect()
}

/// Test for transparent DNS proxy by sending a query to a non-DNS IP.
async fn test_transparent_proxy(domain: &str) -> bool {
    // Send DNS query to a random non-DNS IP — if we get a response, there's a proxy
    let test_ip = "198.51.100.1"; // TEST-NET-2, should not run DNS
    let probe_name = format!("probe-tproxy-{:08x}.{}", rand::random::<u32>(), domain);
    let query = match sp_transport::build_dns_txt_query(&probe_name) {
        Ok(q) => q,
        Err(_) => return false,
    };

    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(_) => return false,
    };

    let addr = format!("{}:53", test_ip);
    if socket.connect(&addr).await.is_err() {
        return false;
    }

    if socket.send(&query.wire).await.is_err() {
        return false;
    }

    let mut buf = vec![0u8; 4096];
    match tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {
            debug!("Transparent proxy detected: got {} bytes from non-DNS IP {}", n, test_ip);
            true
        }
        _ => false,
    }
}

/// Test for DNS hijacking by checking if example.com resolves to its known IP.
async fn test_dns_hijack(system_dns: &[String]) -> bool {
    let expected_ip = "93.184.215.14";
    let qname = "example.com";

    // Build an A query for example.com
    let query = match sp_transport::build_dns_query(qname) {
        Ok(q) => q,
        Err(_) => return false,
    };

    for dns_ip in system_dns {
        let socket = match UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let addr = format!("{}:53", dns_ip);
        if socket.connect(&addr).await.is_err() {
            continue;
        }

        if socket.send(&query.wire).await.is_err() {
            continue;
        }

        let mut buf = vec![0u8; 4096];
        let n = match tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => continue,
        };

        // Parse response and check A records
        use hickory_proto::op::Message;
        use hickory_proto::rr::RecordType;
        use hickory_proto::serialize::binary::BinDecodable;

        if let Ok(msg) = Message::from_bytes(&buf[..n]) {
            for answer in msg.answers() {
                if answer.record_type() == RecordType::A {
                    let ip_str = answer.data().to_string();
                    if ip_str != expected_ip {
                        debug!("DNS hijack detected: example.com resolved to {} (expected {})", ip_str, expected_ip);
                        return true;
                    }
                    // Got expected IP, no hijack for this server
                    return false;
                }
            }
        }
    }

    false
}

/// Test port reachability: UDP 53, TCP 53, HTTPS 443, DoT 853.
async fn test_port_reachability() -> (bool, bool, bool, bool) {
    let (udp53, tcp53, https443, tls853) = tokio::join!(
        test_udp53(),
        test_tcp53(),
        test_https443(),
        test_tls853(),
    );
    (udp53, tcp53, https443, tls853)
}

/// Test UDP port 53 by sending a DNS query to 8.8.8.8.
async fn test_udp53() -> bool {
    let query = match sp_transport::build_dns_query("example.com") {
        Ok(q) => q,
        Err(_) => return false,
    };

    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(_) => return false,
    };

    if socket.connect("8.8.8.8:53").await.is_err() {
        return false;
    }

    if socket.send(&query.wire).await.is_err() {
        return false;
    }

    let mut buf = vec![0u8; 4096];
    matches!(
        tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await,
        Ok(Ok(n)) if n > 0
    )
}

/// Test TCP port 53 by connecting to 8.8.8.8:53 and sending a DNS query.
async fn test_tcp53() -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let stream = match tokio::time::timeout(
        Duration::from_secs(3),
        TcpStream::connect("8.8.8.8:53"),
    )
    .await
    {
        Ok(Ok(s)) => s,
        _ => return false,
    };

    let query = match sp_transport::build_dns_query("example.com") {
        Ok(q) => q,
        Err(_) => return false,
    };

    // TCP DNS: 2-byte length prefix + query
    let len = query.wire.len() as u16;
    let mut tcp_msg = Vec::with_capacity(2 + query.wire.len());
    tcp_msg.extend_from_slice(&len.to_be_bytes());
    tcp_msg.extend_from_slice(&query.wire);

    let (mut reader, mut writer) = tokio::io::split(stream);

    if writer.write_all(&tcp_msg).await.is_err() {
        return false;
    }

    let mut len_buf = [0u8; 2];
    match tokio::time::timeout(Duration::from_secs(3), reader.read_exact(&mut len_buf)).await {
        Ok(Ok(_)) => {
            let resp_len = u16::from_be_bytes(len_buf) as usize;
            resp_len > 0
        }
        _ => false,
    }
}

/// Test HTTPS port 443 by making a DoH query to Cloudflare.
async fn test_https443() -> bool {
    let query = match sp_transport::build_dns_query("example.com") {
        Ok(q) => q,
        Err(_) => return false,
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    match client
        .post("https://cloudflare-dns.com/dns-query")
        .header("Content-Type", "application/dns-message")
        .header("Accept", "application/dns-message")
        .body(query.wire.to_vec())
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Test DoT port 853 by attempting a TLS connection to 1.1.1.1:853.
async fn test_tls853() -> bool {
    match tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::TcpStream::connect("1.1.1.1:853"),
    )
    .await
    {
        Ok(Ok(_)) => true,
        _ => false,
    }
}

/// Test for captive portal via Apple's hotspot detection URL.
async fn test_captive_portal() -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none()) // Don't follow redirects
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Try Google's connectivity check (expect 204)
    match client
        .get("http://connectivitycheck.gstatic.com/generate_204")
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status().as_u16() == 204 {
                return false; // No captive portal
            }
            debug!("Captive portal detected: got HTTP {} instead of 204", resp.status());
            true
        }
        Err(e) => {
            debug!("Captive portal check failed: {}", e);
            // Network error doesn't necessarily mean captive portal
            false
        }
    }
}

fn print_report(report: &NetworkReport) {
    println!("\n=== Network Report ===");
    println!(
        "System DNS:        {}",
        if report.system_dns.is_empty() {
            "none found".to_string()
        } else {
            report.system_dns.join(", ")
        }
    );
    println!(
        "Real resolvers:    {}",
        if report.real_resolvers.is_empty() {
            "none discovered".to_string()
        } else {
            format!("{} (discovered via DNS leak)", report.real_resolvers.join(", "))
        }
    );
    println!(
        "Transparent proxy: {}",
        if report.transparent_proxy { "YES" } else { "NO" }
    );
    println!(
        "DNS hijacking:     {}",
        if report.dns_hijack { "YES" } else { "NO" }
    );
    println!(
        "Captive portal:    {}",
        if report.captive_portal { "YES" } else { "NO" }
    );
    println!(
        "Port 53 (UDP):     {}",
        if report.port53_udp { "OPEN" } else { "BLOCKED" }
    );
    println!(
        "Port 53 (TCP):     {}",
        if report.port53_tcp { "OPEN" } else { "BLOCKED" }
    );
    println!(
        "Port 443 (HTTPS):  {}",
        if report.port443_https { "OPEN" } else { "BLOCKED" }
    );
    println!(
        "Port 853 (DoT):    {}",
        if report.port853_tls { "OPEN" } else { "BLOCKED" }
    );

    // Recommendations
    println!("\nRecommended paths:");
    let mut rec = 1;
    if report.port443_https {
        println!("  {}. DoH via cloudflare-dns.com (HTTPS, invisible)", rec);
        rec += 1;
    }
    for ip in &report.system_dns {
        println!("  {}. System DNS {} (direct, no rate limit)", rec, ip);
        rec += 1;
    }
    for ip in &report.real_resolvers {
        println!("  {}. Real resolver {} (direct, lower latency)", rec, ip);
        rec += 1;
    }
    if rec == 1 {
        println!("  No working paths detected — try providing hints to bootstrap");
    }
}

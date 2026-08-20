// Phase 2: Help user find hints for bootstrap.
// Discovers system DNS, gateways, and suggests known DoH endpoints.

use anyhow::Result;

use crate::detect;

/// Run the hints command: suggest bootstrap hints based on detected network.
pub async fn run_hints(domain: &str) -> Result<()> {
    println!("=== Bootstrap Hints ===\n");

    // 1. System DNS
    let system_dns = detect::read_system_dns();
    if system_dns.is_empty() {
        println!("No system DNS servers detected.");
    } else {
        println!("Detected system DNS:");
        for ip in &system_dns {
            println!("  {} → try: dns-scanner bootstrap --hint dns:{} --domain {}", ip, ip, domain);
        }
    }

    // 2. Gateway IPs
    let gateways = detect_gateways();
    if !gateways.is_empty() {
        println!("\nDetected gateways:");
        for ip in &gateways {
            println!("  {} → try: dns-scanner bootstrap --hint dns:{} --domain {}", ip, ip, domain);
        }
    }

    // 3. Known DoH endpoints
    println!("\nKnown DoH endpoints to try:");
    println!("  dns-scanner bootstrap --hint doh:https://cloudflare-dns.com/dns-query --domain {}", domain);
    println!("  dns-scanner bootstrap --hint doh:https://dns.google/dns-query --domain {}", domain);
    println!("  dns-scanner bootstrap --hint doh:https://dns.quad9.net/dns-query --domain {}", domain);
    println!("  dns-scanner bootstrap --hint doh:https://doh.opendns.com/dns-query --domain {}", domain);

    // 4. Site hint
    println!("\nIf you know a website that works in your network:");
    println!("  dns-scanner bootstrap --hint site:https://example.com --domain {}", domain);
    println!("  dns-scanner bootstrap --hint site:https://google.com --domain {}", domain);

    // 5. Multiple hints
    println!("\nYou can combine multiple hints:");
    let first_dns = system_dns.first().map(|s| s.as_str()).unwrap_or("10.0.0.1");
    println!(
        "  dns-scanner bootstrap --hint dns:{} --hint doh:https://cloudflare-dns.com/dns-query --domain {}",
        first_dns, domain
    );

    Ok(())
}

/// Detect gateway IPs from system routing table (Linux).
fn detect_gateways() -> Vec<String> {
    let mut gateways = Vec::new();

    // Parse "ip route show default" output
    if let Ok(output) = std::process::Command::new("ip")
        .args(["route", "show", "default"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            // Format: "default via 192.168.1.1 dev eth0 ..."
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[0] == "default" && parts[1] == "via" {
                let ip = parts[2].to_string();
                if !gateways.contains(&ip) {
                    gateways.push(ip);
                }
            }
        }
    }

    // Fallback: try common gateway IPs if none found
    if gateways.is_empty() {
        // Don't add these as they overlap with system DNS fallbacks
        // Only add if we actually found nothing
        for candidate in &["192.168.1.1", "192.168.0.1", "10.0.0.1"] {
            gateways.push(candidate.to_string());
        }
    }

    gateways
}

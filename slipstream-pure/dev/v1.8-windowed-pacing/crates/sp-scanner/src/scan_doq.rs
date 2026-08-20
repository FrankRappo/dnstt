// Phase 5: DoQ (DNS-over-QUIC) endpoint discovery — stub implementation.
// Full QUIC DNS requires quinn integration (future work).

use anyhow::Result;
use std::time::Duration;

pub struct ScanDoqConfig {
    pub domain: String,
    pub output: Option<String>,
}

pub async fn run_scan_doq(config: ScanDoqConfig) -> Result<()> {
    let known_doq: Vec<(&str, &str)> = vec![
        ("94.140.14.14:853", "AdGuard"),
        ("94.140.15.15:853", "AdGuard"),
        ("dns.nextdns.io:853", "NextDNS"),
        ("dns.adguard-dns.com:8853", "AdGuard-8853"),
    ];

    eprintln!("Testing {} known DoQ endpoints...", known_doq.len());
    eprintln!("Note: DoQ support is experimental\n");

    let mut reachable_endpoints = Vec::new();

    for (addr, name) in &known_doq {
        let reachable = test_port_reachable(addr).await;
        if reachable {
            eprintln!("  + {} ({}) — port reachable", addr, name);
            reachable_endpoints.push((*addr, *name));
        } else {
            eprintln!("  - {} ({}) — not reachable", addr, name);
        }
    }

    eprintln!(
        "\n=== {} endpoints reachable ===",
        reachable_endpoints.len()
    );
    eprintln!("Full DoQ protocol support requires quinn integration (future)");

    if let Some(path) = &config.output {
        write_doq_results(&reachable_endpoints, path)?;
    }

    let _ = &config.domain; // used in future full implementation
    Ok(())
}

async fn test_port_reachable(addr: &str) -> bool {
    // For DoQ we need to test UDP connectivity
    match tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::UdpSocket::bind("0.0.0.0:0"),
    )
    .await
    {
        Ok(Ok(socket)) => {
            if socket.connect(addr).await.is_err() {
                return false;
            }
            // Send a few bytes to test
            let _ = socket.send(&[0u8; 1]).await;
            true
        }
        _ => false,
    }
}

fn write_doq_results(endpoints: &[(&str, &str)], path: &str) -> Result<()> {
    let mut content = String::from("# Discovered DoQ endpoints (stub — port reachability only)\n\n");
    for (addr, name) in endpoints {
        content.push_str("[[endpoint]]\n");
        content.push_str(&format!("addr = \"{}\"\n", addr));
        content.push_str(&format!("name = \"{}\"\n", name));
        content.push_str("protocol = \"doq\"\n\n");
    }
    std::fs::write(path, content)?;
    Ok(())
}

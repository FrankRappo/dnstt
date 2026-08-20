// Phase 5: DoH endpoint discovery — CT logs + subdomain brute-force.

use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub struct ScanDohConfig {
    pub domain: String,
    pub output: Option<String>,
}

struct DohEndpoint {
    url: String,
    latency_ms: u64,
    delegation_works: bool,
}

pub async fn run_scan_doh(config: ScanDohConfig) -> Result<()> {
    let mut found: Vec<DohEndpoint> = Vec::new();

    // Source 1: Certificate Transparency logs via crt.sh
    eprintln!("[1/3] Searching Certificate Transparency logs (crt.sh)...");
    let ct_domains = search_ct_logs().await;
    eprintln!("  Found {} candidate domains from CT logs", ct_domains.len());

    // Source 2: Brute-force common DNS subdomains on known providers
    eprintln!("[2/3] Brute-forcing DNS subdomains...");
    let brute_domains = brute_dns_subdomains();
    eprintln!(
        "  Generated {} candidates from brute-force",
        brute_domains.len()
    );

    // Source 3: Combine and deduplicate
    let mut all_candidates: Vec<String> = Vec::new();
    all_candidates.extend(ct_domains);
    all_candidates.extend(brute_domains);
    all_candidates.sort();
    all_candidates.dedup();
    eprintln!(
        "[3/3] Testing {} unique DoH candidates...",
        all_candidates.len()
    );

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
            eprintln!(
                "  + FOUND: {} ({}ms)",
                endpoint.url, endpoint.latency_ms
            );
            found.push(endpoint);
        }
    }

    eprintln!("\n=== Found {} working DoH endpoints ===", found.len());
    for ep in &found {
        eprintln!(
            "  {} — {}ms, delegation={}",
            ep.url, ep.latency_ms, ep.delegation_works
        );
    }

    // Write output
    if let Some(path) = &config.output {
        write_doh_results(&found, path)?;
    }

    Ok(())
}

/// Search crt.sh for certificates containing DNS-related domains.
async fn search_ct_logs() -> Vec<String> {
    let search_terms = ["dns", "doh", "resolver", "dnscrypt"];
    let mut domains = Vec::new();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    for term in &search_terms {
        let url = format!("https://crt.sh/?q=%25{}%25&output=json", term);

        match client.get(&url).send().await {
            Ok(resp) => {
                if let Ok(text) = resp.text().await {
                    if let Ok(entries) =
                        serde_json::from_str::<Vec<serde_json::Value>>(&text)
                    {
                        for entry in entries.iter().take(500) {
                            if let Some(cn) =
                                entry.get("common_name").and_then(|v| v.as_str())
                            {
                                let cn = cn.trim().trim_start_matches("*.");
                                if looks_like_doh_domain(cn) {
                                    domains.push(cn.to_string());
                                }
                            }
                            if let Some(names) =
                                entry.get("name_value").and_then(|v| v.as_str())
                            {
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
fn brute_dns_subdomains() -> Vec<String> {
    let prefixes = [
        "dns",
        "doh",
        "dns1",
        "dns2",
        "resolver",
        "ns",
        "ns1",
        "ns2",
        "public-dns",
        "freedns",
        "secure-dns",
        "family-dns",
        "adblock-dns",
    ];

    let base_domains = [
        // Major providers likely to have DoH
        "cloudflare.com",
        "google.com",
        "quad9.net",
        "opendns.com",
        "adguard.com",
        "nextdns.io",
        "cleanbrowsing.org",
        "mullvad.net",
        "controld.com",
        "libredns.gr",
        "dns.sb",
        // ISPs and hosting
        "comcast.net",
        "charter.com",
        "cox.net",
        "att.com",
        "verizon.com",
        "t-mobile.com",
        "deutsche-telekom.de",
        "orange.fr",
        "bt.com",
        // Cloud providers
        "amazonaws.com",
        "azure.com",
        "oracle.com",
        "digitalocean.com",
        "vultr.com",
        "linode.com",
        "hetzner.com",
        "ovh.com",
        // Security companies
        "norton.com",
        "safedns.com",
        "yandex.ru",
        "baidu.com",
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
async fn test_doh_candidate(
    domain: &str,
    tunnel_domain: &str,
) -> Result<Option<DohEndpoint>> {
    let paths = ["/dns-query", "/resolve", "/dns", "/query"];

    for path in &paths {
        let url = format!("https://{}{}", domain, path);

        let query = sp_transport::build_dns_txt_query(&format!(
            "probe-{:08x}.{}",
            rand::random::<u32>(),
            tunnel_domain
        ))?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .danger_accept_invalid_certs(true)
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

fn write_doh_results(endpoints: &[DohEndpoint], path: &str) -> Result<()> {
    let mut content = String::from("# Discovered DoH endpoints\n\n");
    for ep in endpoints {
        content.push_str("[[endpoint]]\n");
        content.push_str(&format!("url = \"{}\"\n", ep.url));
        content.push_str(&format!("latency_ms = {}\n", ep.latency_ms));
        content.push_str(&format!("delegation_works = {}\n\n", ep.delegation_works));
    }
    std::fs::write(path, content)?;
    Ok(())
}

// Phase 2: Bootstrap — auto-discover working DNS path for restricted networks.
// Tests user hints, DoH endpoints, system DNS, transparent proxies, and ranks results.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::debug;

use crate::detect;
use crate::prober;
use crate::resolvers;

/// Configuration for bootstrap command.
pub struct BootstrapConfig {
    pub hints: Vec<String>,
    pub domain: String,
    pub output: Option<String>,
}

/// A discovered working DNS path.
#[derive(Debug, Clone)]
pub struct WorkingPath {
    pub method: PathMethod,
    pub resolver_ip: String,
    pub real_resolver_ip: Option<String>,
    pub doh_url: Option<String>,
    pub latency_ms: u64,
    pub rate_limit_estimate: Option<u32>,
    pub delegation_works: bool,
    pub fronting_domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathMethod {
    DoH,
    SystemDns,
    DirectResolver,
    TransparentProxy,
    DomainFronting,
    DoT,
}

impl std::fmt::Display for PathMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathMethod::DoH => write!(f, "DoH"),
            PathMethod::SystemDns => write!(f, "SystemDNS"),
            PathMethod::DirectResolver => write!(f, "Direct"),
            PathMethod::TransparentProxy => write!(f, "TProxy"),
            PathMethod::DomainFronting => write!(f, "Fronting"),
            PathMethod::DoT => write!(f, "DoT"),
        }
    }
}

// --- Hint parsing ---

enum Hint {
    Dns(String),
    Site(String),
    Doh(String),
}

fn parse_hints(raw: &[String]) -> Result<Vec<Hint>> {
    let mut hints = Vec::new();
    for h in raw {
        if let Some(ip) = h.strip_prefix("dns:") {
            hints.push(Hint::Dns(ip.to_string()));
        } else if let Some(url) = h.strip_prefix("site:") {
            hints.push(Hint::Site(url.to_string()));
        } else if let Some(url) = h.strip_prefix("doh:") {
            hints.push(Hint::Doh(url.to_string()));
        } else {
            anyhow::bail!(
                "Invalid hint '{}'. Use dns:IP, site:URL, or doh:URL",
                h
            );
        }
    }
    Ok(hints)
}

// --- Main bootstrap flow ---

pub async fn run_bootstrap(config: BootstrapConfig) -> Result<()> {
    println!("=== Bootstrap: finding working DNS paths ===\n");

    // Step 0: Parse hints
    let parsed_hints = parse_hints(&config.hints)?;

    let mut all_paths: Vec<WorkingPath> = Vec::new();

    // Step 1: Network detection
    println!("[1/5] Analyzing network...");
    let network = detect::analyze_network(&config.domain).await;
    println!();

    // Step 2: Process user hints
    if !parsed_hints.is_empty() {
        println!("[2/5] Testing user hints...");
        let hint_paths = test_hints(&parsed_hints, &config.domain).await;
        all_paths.extend(hint_paths);
        println!();
    } else {
        println!("[2/5] No user hints provided (use --hint to add)");
    }

    // Step 3: Test DoH endpoints
    println!("[3/5] Testing DoH endpoints...");
    let doh_paths = test_doh_endpoints(&config.domain).await;
    all_paths.extend(doh_paths);
    println!();

    // Step 4: Test system DNS + real resolvers
    println!("[4/5] Testing system DNS...");
    let sys_paths = test_system_dns(&network, &config.domain).await;
    all_paths.extend(sys_paths);
    println!();

    // Step 5: Test transparent proxy path
    if network.transparent_proxy {
        println!("[5/5] Testing transparent proxy path...");
        let proxy_paths = test_transparent_proxy_path(&config.domain).await;
        all_paths.extend(proxy_paths);
        println!();
    } else {
        println!("[5/5] No transparent proxy detected, skipping");
    }

    // Rank and select
    all_paths.sort_by_key(|p| p.latency_ms);
    let primary = select_primary(&all_paths);
    let backup = select_backup(&all_paths, &primary);
    let stealth = select_stealth(&all_paths);

    // Print results
    print_bootstrap_results(&all_paths, &primary, &backup, &stealth);

    // Write output if requested
    if let Some(output_path) = &config.output {
        write_resolvers_toml(&all_paths, output_path)?;
    }

    Ok(())
}

// --- Hint testing ---

async fn test_hints(hints: &[Hint], domain: &str) -> Vec<WorkingPath> {
    let mut all_paths = Vec::new();
    for hint in hints {
        let paths = match hint {
            Hint::Dns(ip) => test_dns_hint(ip, domain).await,
            Hint::Site(url) => test_site_hint(url, domain).await,
            Hint::Doh(url) => test_doh_hint(url, domain).await,
        };
        all_paths.extend(paths);
    }
    all_paths
}

/// Test a dns:IP hint — probe, delegation, discover resolver pool.
async fn test_dns_hint(ip: &str, domain: &str) -> Vec<WorkingPath> {
    let mut paths = Vec::new();

    // 1. Alive test
    let probe = prober::probe_resolver(ip, None, domain).await;
    if !probe.alive {
        println!("  dns:{} — not responding", ip);
        return paths;
    }
    println!(
        "  dns:{} — alive ({}ms, {})",
        ip,
        probe.latency_ms.unwrap_or(0),
        probe.method
    );

    // 2. Delegation test (+ DNS leak: discover real resolver)
    let deleg = prober::test_delegation(ip, None, domain).await;
    if deleg.works {
        println!("  dns:{} — delegation WORKS!", ip);
        if let Some(ref real_ip) = deleg.real_resolver_ip {
            println!("  dns:{} — real resolver: {}", ip, real_ip);
        }
        paths.push(WorkingPath {
            method: PathMethod::SystemDns,
            resolver_ip: ip.to_string(),
            real_resolver_ip: deleg.real_resolver_ip.clone(),
            doh_url: None,
            latency_ms: deleg.latency_ms.unwrap_or(0),
            rate_limit_estimate: None,
            delegation_works: true,
            fronting_domain: None,
        });

        // 3. If we discovered a different real resolver, try it directly
        if let Some(real_ip) = &deleg.real_resolver_ip {
            if real_ip != ip {
                let direct_deleg = prober::test_delegation(real_ip, None, domain).await;
                if direct_deleg.works {
                    println!(
                        "  dns:{} — direct to real resolver {} WORKS! ({}ms)",
                        ip,
                        real_ip,
                        direct_deleg.latency_ms.unwrap_or(0)
                    );
                    paths.push(WorkingPath {
                        method: PathMethod::DirectResolver,
                        resolver_ip: real_ip.clone(),
                        real_resolver_ip: Some(real_ip.clone()),
                        doh_url: None,
                        latency_ms: direct_deleg.latency_ms.unwrap_or(0),
                        rate_limit_estimate: None,
                        delegation_works: true,
                        fronting_domain: None,
                    });
                }
            }
        }
    } else {
        println!(
            "  dns:{} — alive but delegation FAILED: {}",
            ip,
            deleg.error.as_deref().unwrap_or("unknown")
        );
    }

    // 4. Send 4 more probes to discover resolver pool
    let mut discovered_ips: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(ref real_ip) = deleg.real_resolver_ip {
        discovered_ips.insert(real_ip.clone());
    }
    for _ in 0..4 {
        let extra = prober::test_delegation(ip, None, domain).await;
        if let Some(ref real_ip) = extra.real_resolver_ip {
            if discovered_ips.insert(real_ip.clone()) {
                println!("  dns:{} — discovered pool member: {}", ip, real_ip);
                // Test direct access
                let direct = prober::test_delegation(real_ip, None, domain).await;
                if direct.works {
                    paths.push(WorkingPath {
                        method: PathMethod::DirectResolver,
                        resolver_ip: real_ip.clone(),
                        real_resolver_ip: Some(real_ip.clone()),
                        doh_url: None,
                        latency_ms: direct.latency_ms.unwrap_or(0),
                        rate_limit_estimate: None,
                        delegation_works: true,
                        fronting_domain: None,
                    });
                }
            }
        }
    }
    if discovered_ips.len() > 1 {
        println!(
            "  dns:{} — resolver pool: {} servers found",
            ip,
            discovered_ips.len()
        );
    }

    paths
}

/// Test a site:URL hint — resolve the site, detect CDN, try domain fronting.
async fn test_site_hint(url_str: &str, domain: &str) -> Vec<WorkingPath> {
    let mut paths = Vec::new();

    // 1. Parse URL
    let parsed = match url::Url::parse(url_str) {
        Ok(u) => u,
        Err(e) => {
            println!("  site:{} — invalid URL: {}", url_str, e);
            return paths;
        }
    };
    let host = match parsed.host_str() {
        Some(h) => h.to_string(),
        None => {
            println!("  site:{} — no host in URL", url_str);
            return paths;
        }
    };

    // 2. Resolve domain through system DNS
    println!("  site:{} — resolving {}...", url_str, host);
    match tokio::net::lookup_host(format!("{}:443", host)).await {
        Ok(addrs) => {
            let ips: Vec<_> = addrs.map(|a| a.ip().to_string()).collect();
            println!("  site:{} — resolves to: {}", url_str, ips.join(", "));
        }
        Err(e) => {
            println!("  site:{} — DNS resolution failed: {}", url_str, e);
            return paths;
        }
    }

    // 3. DNS leak: test system DNS for delegation
    println!("  site:{} — testing system DNS delegation...", url_str);
    let system_dns = detect::read_system_dns();
    for dns_ip in &system_dns {
        let deleg = prober::test_delegation(dns_ip, None, domain).await;
        if deleg.works {
            println!("  site:{} — system DNS {} delegation WORKS", url_str, dns_ip);
            paths.push(WorkingPath {
                method: PathMethod::SystemDns,
                resolver_ip: dns_ip.clone(),
                real_resolver_ip: deleg.real_resolver_ip.clone(),
                doh_url: None,
                latency_ms: deleg.latency_ms.unwrap_or(0),
                rate_limit_estimate: None,
                delegation_works: true,
                fronting_domain: None,
            });
        }
    }

    // 4. CDN detection
    println!("  site:{} — detecting CDN...", url_str);
    if let Some(cdn) = detect_cdn(url_str).await {
        println!("  site:{} — {} CDN detected", url_str, cdn.name);
        if let Some(ref doh_url) = cdn.doh_endpoint {
            println!("  site:{} — DoH available: {}", url_str, doh_url);

            // 5. Test the DoH endpoint directly
            let doh_deleg = prober::test_delegation("", Some(doh_url), domain).await;
            if doh_deleg.works {
                println!("  site:{} — direct DoH {} WORKS", url_str, doh_url);
                paths.push(WorkingPath {
                    method: PathMethod::DoH,
                    resolver_ip: String::new(),
                    real_resolver_ip: doh_deleg.real_resolver_ip,
                    doh_url: Some(doh_url.clone()),
                    latency_ms: doh_deleg.latency_ms.unwrap_or(0),
                    rate_limit_estimate: Some(30),
                    delegation_works: true,
                    fronting_domain: None,
                });
            }

            // 6. Test domain fronting (simplified: Host header approach)
            println!(
                "  site:{} — testing domain fronting via {}...",
                url_str, host
            );
            let fronting = test_domain_fronting(&host, doh_url, domain).await;
            if fronting.works {
                println!("  site:{} — DOMAIN FRONTING WORKS via {}", url_str, host);
                paths.push(WorkingPath {
                    method: PathMethod::DomainFronting,
                    resolver_ip: host.clone(),
                    real_resolver_ip: None,
                    doh_url: Some(doh_url.clone()),
                    latency_ms: fronting.latency_ms,
                    rate_limit_estimate: None,
                    delegation_works: true,
                    fronting_domain: Some(host.clone()),
                });
            } else {
                println!(
                    "  site:{} — domain fronting failed: {}",
                    url_str,
                    fronting.error.as_deref().unwrap_or("unknown")
                );
            }
        } else {
            println!(
                "  site:{} — {} CDN has no DoH for fronting",
                url_str, cdn.name
            );
        }
    } else {
        println!("  site:{} — no CDN detected", url_str);
    }

    paths
}

/// Test a doh:URL hint — directly test DoH delegation.
async fn test_doh_hint(doh_url: &str, domain: &str) -> Vec<WorkingPath> {
    println!("  doh:{} — testing...", doh_url);
    let deleg = prober::test_delegation("", Some(doh_url), domain).await;
    if deleg.works {
        println!(
            "  doh:{} — delegation WORKS ({}ms)",
            doh_url,
            deleg.latency_ms.unwrap_or(0)
        );
        vec![WorkingPath {
            method: PathMethod::DoH,
            resolver_ip: String::new(),
            real_resolver_ip: deleg.real_resolver_ip,
            doh_url: Some(doh_url.to_string()),
            latency_ms: deleg.latency_ms.unwrap_or(0),
            rate_limit_estimate: Some(30),
            delegation_works: true,
            fronting_domain: None,
        }]
    } else {
        println!(
            "  doh:{} — FAILED: {}",
            doh_url,
            deleg.error.as_deref().unwrap_or("unknown")
        );
        vec![]
    }
}

// --- DoH endpoints test ---

async fn test_doh_endpoints(domain: &str) -> Vec<WorkingPath> {
    let all_resolvers = resolvers::bundled_resolvers();
    let doh_resolvers: Vec<_> = all_resolvers
        .iter()
        .filter(|r| r.doh_url.is_some())
        .collect();

    // Deduplicate by DoH URL
    let mut seen_urls: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unique: Vec<(&str, &str)> = Vec::new();
    for r in &doh_resolvers {
        if let Some(url) = r.doh_url {
            if seen_urls.insert(url.to_string()) {
                unique.push((r.name, url));
            }
        }
    }

    println!("  Testing {} unique DoH endpoints...", unique.len());

    let semaphore = Arc::new(Semaphore::new(10));
    let mut tasks = Vec::new();

    for (name, doh_url) in unique {
        let sem = semaphore.clone();
        let domain = domain.to_string();
        let doh_url = doh_url.to_string();
        let name = name.to_string();

        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let deleg = prober::test_delegation("", Some(&doh_url), &domain).await;
            (name, doh_url, deleg)
        }));
    }

    let mut paths = Vec::new();
    for task in tasks {
        match task.await {
            Ok((name, doh_url, deleg)) => {
                if deleg.works {
                    println!(
                        "  + {} ({}) — {}ms",
                        name,
                        doh_url,
                        deleg.latency_ms.unwrap_or(0)
                    );
                    paths.push(WorkingPath {
                        method: PathMethod::DoH,
                        resolver_ip: String::new(),
                        real_resolver_ip: deleg.real_resolver_ip,
                        doh_url: Some(doh_url),
                        latency_ms: deleg.latency_ms.unwrap_or(0),
                        rate_limit_estimate: Some(30),
                        delegation_works: true,
                        fronting_domain: None,
                    });
                }
            }
            Err(e) => debug!("DoH test task failed: {}", e),
        }
    }

    paths
}

// --- System DNS test ---

async fn test_system_dns(
    network: &detect::NetworkReport,
    domain: &str,
) -> Vec<WorkingPath> {
    let mut paths = Vec::new();

    // Test each system DNS server
    for dns_ip in &network.system_dns {
        let deleg = prober::test_delegation(dns_ip, None, domain).await;
        if deleg.works {
            println!(
                "  System DNS {} — delegation WORKS ({}ms)",
                dns_ip,
                deleg.latency_ms.unwrap_or(0)
            );
            paths.push(WorkingPath {
                method: PathMethod::SystemDns,
                resolver_ip: dns_ip.clone(),
                real_resolver_ip: deleg.real_resolver_ip.clone(),
                doh_url: None,
                latency_ms: deleg.latency_ms.unwrap_or(0),
                rate_limit_estimate: None,
                delegation_works: true,
                fronting_domain: None,
            });

            // Try direct to real resolver if different
            if let Some(ref real_ip) = deleg.real_resolver_ip {
                if real_ip != dns_ip {
                    let direct = prober::test_delegation(real_ip, None, domain).await;
                    if direct.works {
                        println!(
                            "  Direct resolver {} — WORKS ({}ms)",
                            real_ip,
                            direct.latency_ms.unwrap_or(0)
                        );
                        paths.push(WorkingPath {
                            method: PathMethod::DirectResolver,
                            resolver_ip: real_ip.clone(),
                            real_resolver_ip: Some(real_ip.clone()),
                            doh_url: None,
                            latency_ms: direct.latency_ms.unwrap_or(0),
                            rate_limit_estimate: None,
                            delegation_works: true,
                            fronting_domain: None,
                        });
                    }
                }
            }
        } else {
            println!(
                "  System DNS {} — delegation FAILED: {}",
                dns_ip,
                deleg.error.as_deref().unwrap_or("unknown")
            );
        }
    }

    // Also test real resolvers discovered via DNS leak
    for real_ip in &network.real_resolvers {
        // Skip if already tested as system DNS
        if network.system_dns.contains(real_ip) {
            continue;
        }
        let deleg = prober::test_delegation(real_ip, None, domain).await;
        if deleg.works {
            println!(
                "  Real resolver {} — delegation WORKS ({}ms)",
                real_ip,
                deleg.latency_ms.unwrap_or(0)
            );
            paths.push(WorkingPath {
                method: PathMethod::DirectResolver,
                resolver_ip: real_ip.clone(),
                real_resolver_ip: Some(real_ip.clone()),
                doh_url: None,
                latency_ms: deleg.latency_ms.unwrap_or(0),
                rate_limit_estimate: None,
                delegation_works: true,
                fronting_domain: None,
            });
        }
    }

    paths
}

// --- Transparent proxy path ---

async fn test_transparent_proxy_path(domain: &str) -> Vec<WorkingPath> {
    // Send probe through transparent proxy (to random IP)
    let test_ip = "198.51.100.1";
    let deleg = prober::test_delegation(test_ip, None, domain).await;
    if deleg.works {
        println!(
            "  Transparent proxy path — delegation WORKS ({}ms)",
            deleg.latency_ms.unwrap_or(0)
        );
        vec![WorkingPath {
            method: PathMethod::TransparentProxy,
            resolver_ip: test_ip.to_string(),
            real_resolver_ip: deleg.real_resolver_ip,
            doh_url: None,
            latency_ms: deleg.latency_ms.unwrap_or(0),
            rate_limit_estimate: None,
            delegation_works: true,
            fronting_domain: None,
        }]
    } else {
        println!("  Transparent proxy path — delegation FAILED");
        vec![]
    }
}

// --- CDN detection ---

struct CdnInfo {
    name: String,
    doh_endpoint: Option<String>,
}

async fn detect_cdn(url: &str) -> Option<CdnInfo> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;

    let resp = client.get(url).send().await.ok()?;
    let headers = resp.headers();

    // Cloudflare: "CF-Ray" header or "Server: cloudflare"
    if headers.contains_key("cf-ray")
        || headers
            .get("server")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .contains("cloudflare")
    {
        return Some(CdnInfo {
            name: "Cloudflare".to_string(),
            doh_endpoint: Some("https://cloudflare-dns.com/dns-query".to_string()),
        });
    }

    // Google: "Server: gws"
    if let Some(server) = headers.get("server") {
        let s = server.to_str().unwrap_or("");
        if s.contains("gws") || s.contains("Google") {
            return Some(CdnInfo {
                name: "Google".to_string(),
                doh_endpoint: Some("https://dns.google/dns-query".to_string()),
            });
        }
    }

    // Akamai: "X-Akamai-*" headers
    for key in headers.keys() {
        if key.as_str().starts_with("x-akamai") {
            return Some(CdnInfo {
                name: "Akamai".to_string(),
                doh_endpoint: None,
            });
        }
    }

    // Fastly: "Via: ... varnish"
    if let Some(via) = headers.get("via") {
        if via.to_str().unwrap_or("").contains("varnish") {
            return Some(CdnInfo {
                name: "Fastly".to_string(),
                doh_endpoint: None,
            });
        }
    }

    // Amazon CloudFront: "X-Amz-Cf-*" headers
    for key in headers.keys() {
        if key.as_str().starts_with("x-amz-cf") {
            return Some(CdnInfo {
                name: "CloudFront".to_string(),
                doh_endpoint: None,
            });
        }
    }

    None
}

// --- Domain fronting test ---

struct FrontingResult {
    works: bool,
    latency_ms: u64,
    error: Option<String>,
}

async fn test_domain_fronting(
    _fronting_host: &str,
    doh_url: &str,
    domain: &str,
) -> FrontingResult {
    // Build DNS probe query
    let probe_id: u32 = rand::random();
    let probe_name = format!("probe-{:08x}.{}", probe_id, domain);
    let query = match sp_transport::build_dns_txt_query(&probe_name) {
        Ok(q) => q,
        Err(e) => {
            return FrontingResult {
                works: false,
                latency_ms: 0,
                error: Some(format!("build query: {}", e)),
            };
        }
    };

    let doh_parsed = match url::Url::parse(doh_url) {
        Ok(u) => u,
        Err(e) => {
            return FrontingResult {
                works: false,
                latency_ms: 0,
                error: Some(format!("parse URL: {}", e)),
            };
        }
    };
    let doh_host = match doh_parsed.host_str() {
        Some(h) => h.to_string(),
        None => {
            return FrontingResult {
                works: false,
                latency_ms: 0,
                error: Some("no host in DoH URL".to_string()),
            };
        }
    };

    // Simplified domain fronting: set Host header to DoH host
    // Full SNI override requires custom TLS setup — implemented later
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return FrontingResult {
                works: false,
                latency_ms: 0,
                error: Some(format!("build client: {}", e)),
            };
        }
    };

    let start = std::time::Instant::now();
    let result = client
        .post(doh_url)
        .header("Host", &doh_host)
        .header("Content-Type", "application/dns-message")
        .header("Accept", "application/dns-message")
        .body(query.wire.to_vec())
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.bytes().await.unwrap_or_default();
            let latency = start.elapsed().as_millis() as u64;
            match sp_transport::parse_dns_response(&body) {
                Ok(dns_resp) if dns_resp.txt_data.is_some() => FrontingResult {
                    works: true,
                    latency_ms: latency,
                    error: None,
                },
                _ => FrontingResult {
                    works: false,
                    latency_ms: latency,
                    error: Some("no TXT in response".into()),
                },
            }
        }
        Ok(resp) => FrontingResult {
            works: false,
            latency_ms: 0,
            error: Some(format!("HTTP {}", resp.status())),
        },
        Err(e) => FrontingResult {
            works: false,
            latency_ms: 0,
            error: Some(e.to_string()),
        },
    }
}

// --- Selection logic ---

fn select_primary(paths: &[WorkingPath]) -> Option<WorkingPath> {
    // Best overall: working + lowest latency (already sorted)
    paths.iter().find(|p| p.delegation_works).cloned()
}

fn select_backup(
    paths: &[WorkingPath],
    primary: &Option<WorkingPath>,
) -> Option<WorkingPath> {
    // Different method type than primary for redundancy
    let primary_method = primary.as_ref().map(|p| &p.method);
    paths
        .iter()
        .find(|p| p.delegation_works && Some(&p.method) != primary_method)
        .cloned()
}

fn select_stealth(paths: &[WorkingPath]) -> Option<WorkingPath> {
    // Prefer: DomainFronting > DoH > DoT > others
    paths
        .iter()
        .filter(|p| p.delegation_works)
        .find(|p| p.method == PathMethod::DomainFronting)
        .or_else(|| {
            paths
                .iter()
                .find(|p| p.delegation_works && p.method == PathMethod::DoH)
        })
        .cloned()
}

// --- Output ---

fn print_bootstrap_results(
    all_paths: &[WorkingPath],
    primary: &Option<WorkingPath>,
    backup: &Option<WorkingPath>,
    stealth: &Option<WorkingPath>,
) {
    let working: Vec<_> = all_paths
        .iter()
        .filter(|p| p.delegation_works)
        .collect();

    println!("\n=== Bootstrap Results ===");
    println!("Found {} working paths:\n", working.len());

    for (i, path) in working.iter().enumerate() {
        let target = path
            .doh_url
            .as_deref()
            .unwrap_or(&path.resolver_ip);
        println!("  {}. {} — {} ({}ms)", i + 1, path.method, target, path.latency_ms);
        if let Some(ref real) = path.real_resolver_ip {
            println!("     real resolver: {}", real);
        }
        if let Some(ref front) = path.fronting_domain {
            println!("     fronting via: {}", front);
        }
    }

    println!();
    if let Some(p) = primary {
        let target = p.doh_url.as_deref().unwrap_or(&p.resolver_ip);
        println!("PRIMARY: {} — {} ({}ms)", p.method, target, p.latency_ms);
    }
    if let Some(p) = backup {
        let target = p.doh_url.as_deref().unwrap_or(&p.resolver_ip);
        println!("BACKUP:  {} — {} ({}ms)", p.method, target, p.latency_ms);
    }
    if let Some(p) = stealth {
        let target = p.doh_url.as_deref().unwrap_or(&p.resolver_ip);
        println!("STEALTH: {} — {} ({}ms)", p.method, target, p.latency_ms);
    }

    if working.is_empty() {
        println!("\nNo working paths found!");
        println!("Try providing hints: dns-scanner bootstrap --hint dns:YOUR_DNS_IP");
        println!("Or: dns-scanner bootstrap --hint site:https://ANY_WORKING_SITE");
    }
}

fn write_resolvers_toml(paths: &[WorkingPath], output_path: &str) -> Result<()> {
    let mut content = String::from("# Auto-discovered resolvers\n\n");
    for path in paths.iter().filter(|p| p.delegation_works) {
        content.push_str("[[resolver]]\n");
        let name = path
            .doh_url
            .as_deref()
            .unwrap_or(&path.resolver_ip);
        content.push_str(&format!("name = \"{}\"\n", name));
        if !path.resolver_ip.is_empty() {
            content.push_str(&format!("ip = [\"{}\"]\n", path.resolver_ip));
        }
        if let Some(ref doh) = path.doh_url {
            content.push_str(&format!("doh = \"{}\"\n", doh));
        }
        content.push_str(&format!("method = \"{}\"\n", path.method));
        content.push_str(&format!("# latency = {}ms\n", path.latency_ms));
        content.push('\n');
    }
    std::fs::write(output_path, content)?;
    println!("\nWritten to {}", output_path);
    Ok(())
}

# Task: DNS Scanner — Phase 2 (Bootstrap + Hints + Detect)

## Project
`/work/vpn/slipstream-pure-v1.8-windowed-pacing/`

Rust workspace. Phase 1 is complete: sp-scanner crate exists with scan/validate/test working.

## Read FIRST
- `crates/sp-scanner/PLAN.md` — full plan, read Phase 6 (Smart Client Bootstrap) in detail
- `crates/sp-scanner/src/main.rs` — CLI structure, Bootstrap/Hints/Detect commands exist as placeholders
- `crates/sp-scanner/src/prober.rs` — DoH+UDP prober, delegation test, parse_probe_response — REUSE these
- `crates/sp-scanner/src/resolvers.rs` — bundled resolver list
- `crates/sp-scanner/src/bootstrap.rs` — placeholder, replace
- `crates/sp-scanner/src/hints.rs` — placeholder, replace
- `crates/sp-scanner/src/detect.rs` — placeholder, replace
- `crates/sp-transport/src/doh.rs` — DoH transport for reference

## What to build

### 1. Update CLI (main.rs)

Update Bootstrap command to accept hints:
```rust
Bootstrap {
    /// Hints: dns:IP, site:URL, doh:URL (can specify multiple)
    #[arg(long, num_args = 1..)]
    hint: Vec<String>,

    /// Domain for tunnel
    #[arg(long, default_value = "tunnel.example.com")]
    domain: String,
},
```

Update Detect to accept domain:
```rust
Detect {
    #[arg(long, default_value = "tunnel.example.com")]
    domain: String,
},
```

Update Hints to accept domain:
```rust
Hints {
    #[arg(long, default_value = "tunnel.example.com")]
    domain: String,
},
```

Pass domain and hints from CLI to the handler functions.

### 2. Network Detection (detect.rs)

`dns-scanner detect` — analyze network environment and report what works.

Implement:

```rust
pub struct NetworkReport {
    pub system_dns: Vec<String>,           // IPs from OS config
    pub real_resolvers: Vec<String>,       // discovered via DNS leak technique
    pub transparent_proxy: bool,           // transparent DNS proxy detected
    pub dns_hijack: bool,                  // DNS responses manipulated
    pub port53_udp: bool,                  // can reach external DNS on UDP 53
    pub port53_tcp: bool,                  // can reach external DNS on TCP 53
    pub port443_https: bool,              // HTTPS works (DoH possible)
    pub port853_tls: bool,                // DoT port reachable
    pub captive_portal: bool,             // captive portal detected
}
```

**Step 1: Read system DNS config**
```rust
fn read_system_dns() -> Vec<String> {
    // Linux: parse /etc/resolv.conf for "nameserver" lines
    // Windows: parse output of "ipconfig /all" or read registry
    // For now: Linux only (cross-platform later)
    // Also try: gateway IP (common router DNS: 192.168.1.1, 192.168.0.1, 10.0.0.1)
}
```

**Step 2: DNS leak technique — discover real resolvers**
```rust
async fn discover_real_resolvers(domain: &str) -> Vec<String> {
    // Send 5 probe queries: probe-<random>.domain through system DNS
    // Use the OS default resolver (not specifying target IP — use system's)
    // Parse TXT response to get "resolver=<ip>"
    // Deduplicate — may get 3-5 unique IPs from ISP pool
    // Use tokio::net::UdpSocket bound to 0.0.0.0:0
    // Send to system DNS IPs from step 1
}
```

**Step 3: Transparent proxy test**
```rust
async fn test_transparent_proxy(domain: &str) -> (bool, Option<String>) {
    // Send DNS query to random non-DNS IP (e.g., 198.51.100.1:53)
    // If response comes → transparent proxy active
    // Also send probe query through this path → get real resolver IP
    // Return: (proxy_detected, real_resolver_ip)
}
```

**Step 4: DNS hijack test**
```rust
async fn test_dns_hijack() -> bool {
    // Query A record for "example.com" through system DNS
    // Expected IP: 93.184.215.14 (well-known)
    // If different → responses being manipulated
    // BUT: this may only affect blacklisted domains, our tunnel may still work
}
```

**Step 5: Port reachability**
```rust
async fn test_port_reachability() -> (bool, bool, bool, bool) {
    // UDP 53: try to reach 8.8.8.8:53 with DNS query
    // TCP 53: try TCP connection to 8.8.8.8:53, send DNS query
    // HTTPS 443: try DoH to https://cloudflare-dns.com/dns-query
    // TLS 853: try TLS connection to 1.1.1.1:853 (DoT)
    // Return: (udp53, tcp53, https443, tls853)
}
```

**Step 6: Captive portal test**
```rust
async fn test_captive_portal() -> bool {
    // HTTP GET to http://captive.apple.com/hotspot-detect.html
    // Expected: "<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>"
    // If different → captive portal
    // Alternative: http://connectivitycheck.gstatic.com/generate_204 → expect 204
}
```

**Output**: print report to stdout:
```
=== Network Analysis ===
System DNS:        10.0.0.1, 10.0.0.2
Real resolvers:    85.21.192.3, 85.21.192.4 (discovered via DNS leak)
Transparent proxy: NO
DNS hijacking:     NO
Captive portal:    NO
Port 53 (UDP):     OPEN
Port 53 (TCP):     OPEN
Port 443 (HTTPS):  OPEN
Port 853 (DoT):    BLOCKED

Recommended paths:
  1. DoH via cloudflare-dns.com (HTTPS, invisible)
  2. System DNS 10.0.0.1 (direct, no rate limit)
  3. Real resolver 85.21.192.3 (direct, lower latency)
```

### 3. Hints Helper (hints.rs)

`dns-scanner hints` — help user find hints for bootstrap.

```rust
pub async fn run_hints(domain: &str) -> Result<()> {
    // 1. Read system DNS → suggest --hint dns:IP
    let system_dns = read_system_dns();
    println!("Detected system DNS:");
    for ip in &system_dns {
        println!("  {} → try: dns-scanner bootstrap --hint dns:{}", ip, ip);
    }

    // 2. Detect gateway IPs → suggest as possible DNS
    let gateways = detect_gateways();  // parse "ip route" or similar
    println!("\nDetected gateways:");
    for ip in &gateways {
        println!("  {} → try: dns-scanner bootstrap --hint dns:{}", ip, ip);
    }

    // 3. Suggest known DoH endpoints
    println!("\nKnown DoH endpoints to try:");
    println!("  dns-scanner bootstrap --hint doh:https://cloudflare-dns.com/dns-query");
    println!("  dns-scanner bootstrap --hint doh:https://dns.google/dns-query");
    println!("  dns-scanner bootstrap --hint doh:https://dns.quad9.net/dns-query");

    // 4. Suggest site hint
    println!("\nIf you know a website that works in your network:");
    println!("  dns-scanner bootstrap --hint site:https://example.com");

    Ok(())
}

fn detect_gateways() -> Vec<String> {
    // Linux: parse "ip route show default" → extract gateway IP
    // Also try common gateways: 192.168.1.1, 192.168.0.1, 10.0.0.1
}
```

### 4. Bootstrap (bootstrap.rs) — THE MAIN PIECE

`dns-scanner bootstrap` — auto-discover working DNS path.

This is the most important command. It must work on restricted networks.

#### Structure:
```rust
pub struct BootstrapConfig {
    pub hints: Vec<String>,       // from --hint flags
    pub domain: String,
    pub output: Option<String>,
}

pub struct BootstrapResult {
    pub paths: Vec<WorkingPath>,  // all discovered working paths
    pub primary: Option<WorkingPath>,   // best overall
    pub backup: Option<WorkingPath>,    // different type for redundancy
    pub stealth: Option<WorkingPath>,   // most hidden (domain fronting)
    pub network: NetworkReport,         // from detect
}

pub struct WorkingPath {
    pub method: PathMethod,
    pub resolver_ip: String,
    pub real_resolver_ip: Option<String>,
    pub doh_url: Option<String>,
    pub latency_ms: u64,
    pub rate_limit_estimate: Option<u32>,  // estimated max rps
    pub delegation_works: bool,
    pub fronting_domain: Option<String>,   // for domain fronting
}

pub enum PathMethod {
    DoH,              // HTTPS DoH endpoint
    SystemDns,        // OS-configured DNS (may be forwarder)
    DirectResolver,   // Direct UDP to real resolver
    TransparentProxy, // DNS through transparent proxy
    DomainFronting,   // DoH via CDN domain fronting
    DoT,              // DNS-over-TLS port 853
}
```

#### Flow:

```rust
pub async fn run_bootstrap(config: BootstrapConfig) -> Result<()> {
    println!("=== Bootstrap: finding working DNS paths ===\n");

    // Step 0: Parse hints
    let parsed_hints = parse_hints(&config.hints)?;
    // Returns Vec<Hint> where Hint is DnsHint{ip}, SiteHint{url}, DohHint{url}

    let mut all_paths: Vec<WorkingPath> = Vec::new();

    // Step 1: Run network detection (reuse detect.rs logic)
    println!("[1/5] Analyzing network...");
    let network = detect::analyze_network(&config.domain).await;
    // Print summary

    // Step 2: Process hints IN PARALLEL
    if !parsed_hints.is_empty() {
        println!("[2/5] Testing user hints...");
        let hint_paths = test_hints_parallel(&parsed_hints, &config.domain).await;
        all_paths.extend(hint_paths);
    }

    // Step 3: Test DoH endpoints (stealth, invisible to ISP)
    println!("[3/5] Testing DoH endpoints...");
    let doh_paths = test_doh_endpoints(&config.domain).await;
    all_paths.extend(doh_paths);

    // Step 4: Test system DNS + real resolvers from DNS leak
    println!("[4/5] Testing system DNS...");
    let sys_paths = test_system_dns(&network, &config.domain).await;
    all_paths.extend(sys_paths);

    // Step 5: Test transparent proxy (if detected)
    if network.transparent_proxy {
        println!("[5/5] Testing transparent proxy...");
        let proxy_paths = test_transparent_proxy_path(&config.domain).await;
        all_paths.extend(proxy_paths);
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
```

#### Hint processing:

**dns:IP hint**
```rust
async fn test_dns_hint(ip: &str, domain: &str) -> Vec<WorkingPath> {
    let mut paths = Vec::new();

    // 1. Alive test
    let probe = prober::probe_resolver(ip, None, domain).await;
    if !probe.alive {
        println!("  {} — not responding", ip);
        return paths;
    }
    println!("  {} — alive ({}ms, {})", ip, probe.latency_ms.unwrap_or(0), probe.method);

    // 2. Delegation test (+ DNS leak: discover real resolver)
    let deleg = prober::test_delegation(ip, None, domain).await;
    if deleg.works {
        println!("  {} — delegation WORKS!", ip);
        if let Some(ref real_ip) = deleg.real_resolver_ip {
            println!("  {} — real resolver: {}", ip, real_ip);
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

        // 3. If we discovered real resolver, try it directly too
        if let Some(real_ip) = &deleg.real_resolver_ip {
            if real_ip != ip {
                let direct_deleg = prober::test_delegation(real_ip, None, domain).await;
                if direct_deleg.works {
                    println!("  {} — direct to real resolver WORKS! ({}ms)", real_ip, direct_deleg.latency_ms.unwrap_or(0));
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
        println!("  {} — alive but delegation FAILED: {:?}", ip, deleg.error);
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
                println!("  {} — discovered pool member: {}", ip, real_ip);
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
        println!("  {} — resolver pool: {} servers found", ip, discovered_ips.len());
    }

    paths
}
```

**site:URL hint**
```rust
async fn test_site_hint(url: &str, domain: &str) -> Vec<WorkingPath> {
    let mut paths = Vec::new();

    // 1. Parse URL to get hostname
    let parsed = url::Url::parse(url)?;  // add "url" crate to dependencies
    let host = parsed.host_str()?;

    // 2. Resolve domain through system DNS (proves DNS works for this domain)
    println!("  Resolving {}...", host);
    let addrs = tokio::net::lookup_host(format!("{}:443", host)).await;
    match addrs {
        Ok(addrs) => {
            let ips: Vec<_> = addrs.map(|a| a.ip().to_string()).collect();
            println!("  {} resolves to: {}", host, ips.join(", "));
        }
        Err(e) => {
            println!("  {} — DNS resolution failed: {}", host, e);
            return paths;
        }
    }

    // 3. DNS leak: simultaneously send probe to discover real resolver
    println!("  Discovering real resolver via DNS leak...");
    // Send probes through system DNS (same path that resolved the site)
    let system_dns = detect::read_system_dns();
    for dns_ip in &system_dns {
        let deleg = prober::test_delegation(dns_ip, None, domain).await;
        if deleg.works {
            println!("  System DNS {} — delegation WORKS", dns_ip);
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

    // 4. CDN detection (connect HTTPS, check headers)
    println!("  Detecting CDN for {}...", host);
    let cdn = detect_cdn(url).await;
    match cdn {
        Some(CdnInfo { name, doh_endpoint: Some(doh_url) }) => {
            println!("  {} is on {} CDN — DoH available: {}", host, name, doh_url);

            // 5. Test domain fronting
            println!("  Testing domain fronting: SNI={} → {}", host, doh_url);
            let fronting_result = test_domain_fronting(host, &doh_url, domain).await;
            if fronting_result.works {
                println!("  DOMAIN FRONTING WORKS via {}", host);
                paths.push(WorkingPath {
                    method: PathMethod::DomainFronting,
                    resolver_ip: host.to_string(),
                    real_resolver_ip: None,
                    doh_url: Some(doh_url),
                    latency_ms: fronting_result.latency_ms,
                    rate_limit_estimate: None,
                    delegation_works: true,
                    fronting_domain: Some(host.to_string()),
                });
            } else {
                println!("  Domain fronting failed: {:?}", fronting_result.error);
            }

            // 6. Also test the DoH endpoint directly (may work without fronting)
            let doh_deleg = prober::test_delegation("", Some(&doh_url), domain).await;
            if doh_deleg.works {
                println!("  Direct DoH {} — WORKS", doh_url);
                paths.push(WorkingPath {
                    method: PathMethod::DoH,
                    resolver_ip: String::new(),
                    real_resolver_ip: doh_deleg.real_resolver_ip,
                    doh_url: Some(doh_url),
                    latency_ms: doh_deleg.latency_ms.unwrap_or(0),
                    rate_limit_estimate: None,
                    delegation_works: true,
                    fronting_domain: None,
                });
            }
        }
        Some(CdnInfo { name, doh_endpoint: None }) => {
            println!("  {} is on {} CDN — no DoH available for fronting", host, name);
        }
        None => {
            println!("  {} — no CDN detected", host);
        }
    }

    paths
}
```

**doh:URL hint**
```rust
async fn test_doh_hint(doh_url: &str, domain: &str) -> Vec<WorkingPath> {
    println!("  Testing DoH: {}...", doh_url);
    let deleg = prober::test_delegation("", Some(doh_url), domain).await;
    if deleg.works {
        println!("  DoH {} — delegation WORKS ({}ms)", doh_url, deleg.latency_ms.unwrap_or(0));
        vec![WorkingPath {
            method: PathMethod::DoH,
            resolver_ip: String::new(),
            real_resolver_ip: deleg.real_resolver_ip,
            doh_url: Some(doh_url.to_string()),
            latency_ms: deleg.latency_ms.unwrap_or(0),
            rate_limit_estimate: None,
            delegation_works: true,
            fronting_domain: None,
        }]
    } else {
        println!("  DoH {} — FAILED: {:?}", doh_url, deleg.error);
        vec![]
    }
}
```

#### CDN Detection:
```rust
struct CdnInfo {
    name: String,
    doh_endpoint: Option<String>,
}

async fn detect_cdn(url: &str) -> Option<CdnInfo> {
    // Connect HTTPS, check response headers
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build().ok()?;

    let resp = client.get(url).send().await.ok()?;
    let headers = resp.headers();

    // Cloudflare: "CF-Ray" header or "Server: cloudflare"
    if headers.contains_key("cf-ray") || headers.get("server").map(|v| v.to_str().unwrap_or("")).unwrap_or("").contains("cloudflare") {
        return Some(CdnInfo {
            name: "Cloudflare".to_string(),
            doh_endpoint: Some("https://cloudflare-dns.com/dns-query".to_string()),
        });
    }

    // Google: check if IP is in Google ranges, or "Server: gws"
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

    // Fastly: "X-Served-By" with cache-* or "Via: ... varnish"
    if let Some(via) = headers.get("via") {
        if via.to_str().unwrap_or("").contains("varnish") {
            return Some(CdnInfo {
                name: "Fastly".to_string(),
                doh_endpoint: None,
            });
        }
    }

    // Amazon CloudFront: "X-Amz-Cf-*" headers or "Via: ... CloudFront"
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
```

#### Domain Fronting Test:
```rust
struct FrontingResult {
    works: bool,
    latency_ms: u64,
    error: Option<String>,
}

async fn test_domain_fronting(fronting_host: &str, doh_url: &str, domain: &str) -> FrontingResult {
    // Build DoH request with different SNI and Host
    // SNI = fronting_host (what DPI sees)
    // Host header = actual DoH host (what CDN routes to)

    let doh_parsed = url::Url::parse(doh_url).unwrap();
    let doh_host = doh_parsed.host_str().unwrap();

    // Build DNS query
    let probe_id: u32 = rand::random();
    let probe_name = format!("probe-{:08x}.{}", probe_id, domain);
    let query = sp_transport::build_dns_txt_query(&probe_name).unwrap();

    // Send HTTPS request with SNI=fronting_host but Host=doh_host
    // reqwest doesn't support SNI override directly.
    // Use rustls with custom ServerName for SNI, connect to fronting_host IP
    // but set Host header to doh_host.
    //
    // Simplified approach: just set Host header (works if CDN routes by Host)
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build().unwrap();

    let start = Instant::now();
    let result = client
        .post(doh_url)
        .header("Host", doh_host)
        .header("Content-Type", "application/dns-message")
        .header("Accept", "application/dns-message")
        .body(query.wire.to_vec())
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.bytes().await.unwrap_or_default();
            let latency = start.elapsed().as_millis() as u64;
            let parsed = sp_transport::parse_dns_response(&body);
            match parsed {
                Ok(dns_resp) if dns_resp.txt_data.is_some() => {
                    FrontingResult { works: true, latency_ms: latency, error: None }
                }
                _ => FrontingResult { works: false, latency_ms: latency, error: Some("no TXT in response".into()) }
            }
        }
        Ok(resp) => FrontingResult { works: false, latency_ms: 0, error: Some(format!("HTTP {}", resp.status())) },
        Err(e) => FrontingResult { works: false, latency_ms: 0, error: Some(e.to_string()) },
    }
}
```

#### DoH endpoints test:
```rust
async fn test_doh_endpoints(domain: &str) -> Vec<WorkingPath> {
    // Get all bundled resolvers that have DoH URLs
    let resolvers = resolvers::bundled_resolvers();
    let doh_resolvers: Vec<_> = resolvers.iter().filter(|r| r.doh_url.is_some()).collect();

    // Deduplicate by DoH URL
    let mut seen_urls: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unique: Vec<_> = Vec::new();
    for r in &doh_resolvers {
        if let Some(url) = r.doh_url {
            if seen_urls.insert(url.to_string()) {
                unique.push(r);
            }
        }
    }

    println!("  Testing {} unique DoH endpoints...", unique.len());

    // Test in parallel (10 concurrent)
    let semaphore = Arc::new(Semaphore::new(10));
    let mut tasks = Vec::new();

    for resolver in unique {
        let sem = semaphore.clone();
        let domain = domain.to_string();
        let doh_url = resolver.doh_url.unwrap().to_string();
        let name = resolver.name.to_string();

        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let deleg = prober::test_delegation("", Some(&doh_url), &domain).await;
            (name, doh_url, deleg)
        }));
    }

    let mut paths = Vec::new();
    for task in tasks {
        if let Ok((name, doh_url, deleg)) = task.await {
            if deleg.works {
                println!("  ✓ {} ({}) — {}ms", name, doh_url, deleg.latency_ms.unwrap_or(0));
                paths.push(WorkingPath {
                    method: PathMethod::DoH,
                    resolver_ip: String::new(),
                    real_resolver_ip: deleg.real_resolver_ip,
                    doh_url: Some(doh_url),
                    latency_ms: deleg.latency_ms.unwrap_or(0),
                    rate_limit_estimate: Some(30), // conservative default
                    delegation_works: true,
                    fronting_domain: None,
                });
            }
        }
    }

    paths
}
```

#### Selection logic:
```rust
fn select_primary(paths: &[WorkingPath]) -> Option<WorkingPath> {
    // Best overall: working + lowest latency
    paths.iter().find(|p| p.delegation_works).cloned()
}

fn select_backup(paths: &[WorkingPath], primary: &Option<WorkingPath>) -> Option<WorkingPath> {
    // Different method than primary for redundancy
    let primary_method = primary.as_ref().map(|p| std::mem::discriminant(&p.method));
    paths.iter()
        .find(|p| p.delegation_works && Some(std::mem::discriminant(&p.method)) != primary_method)
        .cloned()
}

fn select_stealth(paths: &[WorkingPath]) -> Option<WorkingPath> {
    // Prefer: DomainFronting > DoH > DoT > others
    paths.iter()
        .filter(|p| p.delegation_works)
        .find(|p| matches!(p.method, PathMethod::DomainFronting))
        .or_else(|| paths.iter().find(|p| p.delegation_works && matches!(p.method, PathMethod::DoH)))
        .cloned()
}
```

#### Output:
```rust
fn print_bootstrap_results(
    all_paths: &[WorkingPath],
    primary: &Option<WorkingPath>,
    backup: &Option<WorkingPath>,
    stealth: &Option<WorkingPath>,
) {
    let working = all_paths.iter().filter(|p| p.delegation_works).count();
    println!("\n=== Bootstrap Results ===");
    println!("Found {} working paths:\n", working);

    for (i, path) in all_paths.iter().filter(|p| p.delegation_works).enumerate() {
        println!("  {}. {:?} — {} ({}ms)", i + 1,
            path.method,
            path.doh_url.as_deref().unwrap_or(&path.resolver_ip),
            path.latency_ms);
        if let Some(ref real) = path.real_resolver_ip {
            println!("     real resolver: {}", real);
        }
        if let Some(ref front) = path.fronting_domain {
            println!("     fronting via: {}", front);
        }
    }

    println!();
    if let Some(p) = primary {
        println!("PRIMARY: {:?} — {} ({}ms)", p.method,
            p.doh_url.as_deref().unwrap_or(&p.resolver_ip), p.latency_ms);
    }
    if let Some(p) = backup {
        println!("BACKUP:  {:?} — {} ({}ms)", p.method,
            p.doh_url.as_deref().unwrap_or(&p.resolver_ip), p.latency_ms);
    }
    if let Some(p) = stealth {
        println!("STEALTH: {:?} — {} ({}ms)", p.method,
            p.doh_url.as_deref().unwrap_or(&p.resolver_ip), p.latency_ms);
    }

    if working == 0 {
        println!("\nNo working paths found!");
        println!("Try providing hints: dns-scanner bootstrap --hint dns:YOUR_DNS_IP");
        println!("Or: dns-scanner bootstrap --hint site:https://ANY_WORKING_SITE");
    }
}

fn write_resolvers_toml(paths: &[WorkingPath], output_path: &str) -> Result<()> {
    // Write working paths in project resolver TOML format
    let mut content = String::from("# Auto-discovered resolvers\n\n");
    for path in paths.iter().filter(|p| p.delegation_works) {
        content.push_str("[[resolver]]\n");
        let name = path.doh_url.as_deref().unwrap_or(&path.resolver_ip);
        content.push_str(&format!("name = \"{}\"\n", name));
        if !path.resolver_ip.is_empty() {
            content.push_str(&format!("ip = [\"{}\"]\n", path.resolver_ip));
        }
        if let Some(ref doh) = path.doh_url {
            content.push_str(&format!("doh = \"{}\"\n", doh));
        }
        content.push_str(&format!("# latency = {}ms\n", path.latency_ms));
        content.push('\n');
    }
    std::fs::write(output_path, content)?;
    println!("\nWritten to {}", output_path);
    Ok(())
}
```

### 5. Dependencies

Add to `crates/sp-scanner/Cargo.toml` [dependencies]:
```toml
url = "2"
```

No other new dependencies needed — reqwest, tokio, hickory-proto already there.

### 6. Make detect functions public for reuse

`detect.rs` functions like `read_system_dns()` and `analyze_network()` must be `pub`
so `bootstrap.rs` can call them.

### 7. Build and test

1. `cargo build` — zero errors
2. `cargo build --release` — clean
3. `cargo test -p sp-scanner` — all tests pass
4. Test CLI:
   - `dns-scanner detect --domain tunnel.example.com`
   - `dns-scanner hints --domain tunnel.example.com`
   - `dns-scanner bootstrap --domain tunnel.example.com`
   - `dns-scanner bootstrap --hint doh:https://dns.google/dns-query --domain tunnel.example.com`
   - `dns-scanner bootstrap --hint dns:8.8.8.8 --domain tunnel.example.com`
5. Fix any warnings

## Constraints

- Reuse prober.rs functions — don't duplicate DNS query logic
- detect.rs functions must be pub for bootstrap.rs to use
- Linux-first for system DNS parsing, but structure code so Windows/macOS can be added later
- All async with tokio
- Use tracing for logging
- Domain fronting: simplified approach first (Host header), full SNI override later
- Commit when done

## File changes
```
MODIFY: crates/sp-scanner/Cargo.toml        ← add url dependency
MODIFY: crates/sp-scanner/src/main.rs       ← update Bootstrap/Detect/Hints commands with args
REPLACE: crates/sp-scanner/src/bootstrap.rs  ← full implementation
REPLACE: crates/sp-scanner/src/detect.rs     ← full implementation
REPLACE: crates/sp-scanner/src/hints.rs      ← full implementation
```

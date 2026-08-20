# Task: DNS Scanner — Phase 9 (Integration into sp-client)

## Project
`/work/vpn/slipstream-pure-v1.8-windowed-pacing/`

DNS scanner (sp-scanner) is complete with all features. Now integrate scanner
logic into sp-client so the tunnel client auto-discovers resolvers.

## Read FIRST
- `crates/sp-scanner/PLAN.md` — read Phase 9 (Integration) section
- `crates/sp-client/src/main.rs` — full client code (770 lines), understand how it starts:
  1. Loads resolvers.toml config
  2. Creates transport paths (UDP, TCP, DoH, DoH+ECH) for each resolver
  3. Creates MultipathRouter
  4. Discovers good paths
  5. Starts SOCKS5 listener
- `crates/sp-client/Cargo.toml` — current client dependencies
- `crates/sp-scanner/src/bootstrap.rs` — bootstrap logic to reuse
- `crates/sp-scanner/src/detect.rs` — network detection to reuse
- `crates/sp-scanner/src/prober.rs` — probe/delegation functions
- `crates/sp-scanner/src/resolvers.rs` — bundled resolver list
- `crates/sp-scanner/src/scanner.rs` — scan logic with auto mode
- `crates/sp-scanner/src/stealth.rs` — stealth scanning

## Architecture

Currently client loads resolvers from static config file only.
After integration, client will:

```
STARTUP:
  1. Load cached resolvers (if cache file exists from previous run)
  2. Load static config (resolvers.toml) as fallback
  3. Quick test: which cached resolvers still work? (5-10 seconds)
  4. Start tunnel with best available resolvers
  5. In background: run scanner to discover more/better resolvers

BACKGROUND:
  6. Every 5 min: quick test existing resolvers (still alive?)
  7. Every 30 min: scan for new resolvers (bundled list, DoH probes)
  8. When better resolver found: hot-add to router (no restart)
  9. When resolver dies: remove from router, try alternatives
  10. Save working resolvers to cache file periodically

NETWORK CHANGE:
  11. Detect network change (new IP, new gateway, new system DNS)
  12. Re-run bootstrap (find working paths for new network)
  13. Hot-swap resolvers in router
```

## What to build

### 1. Create sp-scanner as library (not just binary)

Currently sp-scanner is only a binary (`[[bin]]`). Need to also expose it as a library
so sp-client can use its modules.

In `crates/sp-scanner/Cargo.toml`, add:
```toml
[lib]
name = "sp_scanner"
path = "src/lib.rs"
```

Create `crates/sp-scanner/src/lib.rs`:
```rust
// Re-export modules needed by sp-client
pub mod bootstrap;
pub mod detect;
pub mod exclude;
pub mod hints;
pub mod monitor;
pub mod prober;
pub mod profile;
pub mod resolvers;
pub mod scanner;
pub mod stealth;
pub mod scan_doh;
pub mod scan_dot;
pub mod scan_doq;
pub mod timing_stego;
```

Note: main.rs currently has `mod` declarations for all these modules. After creating lib.rs,
main.rs should use `use sp_scanner::*;` or import from the library. The simplest approach:
- lib.rs declares all modules as `pub mod`
- main.rs uses `use sp_scanner::{module}` instead of `mod module`

However, to minimize changes to main.rs, you can keep `mod` in main.rs and ALSO have lib.rs.
Cargo supports both `[[bin]]` and `[lib]` in the same crate. The bin will use its own `mod`
declarations, and the lib will expose them for external crates. The modules need to be
accessible from both. Simplest: move `mod` declarations to lib.rs, and in main.rs do
`use sp_scanner::*;` or specific imports.

### 2. Add sp-scanner dependency to sp-client

In `crates/sp-client/Cargo.toml`:
```toml
sp-scanner = { path = "../sp-scanner" }
```

### 3. Resolver cache file (src/resolver_cache.rs in sp-client)

```rust
use anyhow::Result;
use sp_common::config::{Config, ResolverConfig};
use std::path::Path;

const CACHE_FILE: &str = "resolver-cache.toml";

/// Load cached resolvers from previous run.
/// Returns None if cache doesn't exist or is corrupted.
pub fn load_cache(cache_dir: &Path) -> Option<Config> {
    let path = cache_dir.join(CACHE_FILE);
    if !path.exists() { return None; }
    Config::load(&path).ok()
}

/// Save working resolvers to cache file.
pub fn save_cache(cache_dir: &Path, resolvers: &[ResolverConfig]) -> Result<()> {
    let path = cache_dir.join(CACHE_FILE);
    let mut content = String::from("# Auto-discovered resolver cache\n");
    content.push_str(&format!("# Updated: {}\n\n", chrono_or_timestamp()));

    for r in resolvers {
        content.push_str("[[resolver]]\n");
        content.push_str(&format!("name = \"{}\"\n", r.name));
        let ips: Vec<String> = r.ip.iter().map(|ip| format!("\"{}\"", ip)).collect();
        content.push_str(&format!("ip = [{}]\n", ips.join(", ")));
        if let Some(ref doh) = r.doh {
            content.push_str(&format!("doh = \"{}\"\n", doh));
        }
        if r.ech {
            content.push_str("ech = true\n");
        }
        content.push('\n');
    }

    std::fs::create_dir_all(cache_dir)?;
    std::fs::write(&path, content)?;
    Ok(())
}

fn chrono_or_timestamp() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap().as_secs();
    format!("{}", ts)
}
```

### 4. Background scanner task (src/background_scanner.rs in sp-client)

```rust
use anyhow::Result;
use sp_common::config::ResolverConfig;
use sp_multipath::MultipathRouter;
use sp_scanner::prober;
use sp_scanner::resolvers::bundled_resolvers;
use sp_scanner::detect;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{info, warn, debug};

pub struct BackgroundScanner {
    domain: String,
    router: Arc<MultipathRouter>,
    /// Channel to notify main thread of new resolvers
    new_resolver_tx: watch::Sender<Vec<ResolverConfig>>,
    /// Quick test interval (seconds)
    quick_test_interval: u64,
    /// Full scan interval (seconds)
    full_scan_interval: u64,
    /// Cache directory
    cache_dir: std::path::PathBuf,
}

impl BackgroundScanner {
    pub fn new(
        domain: String,
        router: Arc<MultipathRouter>,
        cache_dir: std::path::PathBuf,
    ) -> (Self, watch::Receiver<Vec<ResolverConfig>>) {
        let (tx, rx) = watch::channel(Vec::new());
        let scanner = Self {
            domain,
            router,
            new_resolver_tx: tx,
            quick_test_interval: 300,    // 5 min
            full_scan_interval: 1800,    // 30 min
            cache_dir,
        };
        (scanner, rx)
    }

    /// Run background scanner. Call this with tokio::spawn.
    pub async fn run(self) {
        info!("[bg-scanner] Background scanner started");
        info!("[bg-scanner] Quick test every {}s, full scan every {}s",
            self.quick_test_interval, self.full_scan_interval);

        let mut quick_interval = tokio::time::interval(Duration::from_secs(self.quick_test_interval));
        let mut scan_interval = tokio::time::interval(Duration::from_secs(self.full_scan_interval));
        // Skip first tick (we already tested at startup)
        quick_interval.tick().await;
        scan_interval.tick().await;

        let mut scan_counter = 0u64;

        loop {
            tokio::select! {
                _ = quick_interval.tick() => {
                    debug!("[bg-scanner] Running quick health check");
                    self.quick_health_check().await;
                }
                _ = scan_interval.tick() => {
                    scan_counter += 1;
                    info!("[bg-scanner] Running full scan #{}", scan_counter);
                    self.full_scan().await;
                }
            }
        }
    }

    /// Quick check: test existing resolvers, remove dead ones.
    async fn quick_health_check(&self) {
        let active_count = self.router.active_path_count().await;
        debug!("[bg-scanner] Health check: {} active paths", active_count);

        // Test each active path with a probe
        // If failed → mark as down (router handles this internally via error counting)
        // Just send a probe through the router to keep paths alive
        let probe_name = format!("probe-{:08x}.{}", rand::random::<u32>(), self.domain);
        if let Ok(query) = sp_transport::build_dns_txt_query(&probe_name) {
            match self.router.send(&query).await {
                Ok(_) => debug!("[bg-scanner] Health probe OK"),
                Err(e) => warn!("[bg-scanner] Health probe failed: {}", e),
            }
        }
    }

    /// Full scan: discover new resolvers from bundled list.
    async fn full_scan(&self) {
        let bundled = bundled_resolvers();
        let mut new_found = Vec::new();

        // Test bundled resolvers via DoH (stealth, doesn't use the tunnel)
        for resolver in &bundled {
            if let Some(doh_url) = resolver.doh_url {
                let deleg = prober::test_delegation(
                    resolver.ip, Some(doh_url), &self.domain
                ).await;

                if deleg.works {
                    debug!("[bg-scanner] Found working resolver: {} ({})",
                        resolver.name, resolver.ip);
                    new_found.push(ResolverConfig {
                        name: resolver.name.to_string(),
                        ip: vec![resolver.ip.to_string()],
                        doh: resolver.doh_url.map(|s| s.to_string()),
                        ech: false,
                    });
                }
            }
        }

        if !new_found.is_empty() {
            info!("[bg-scanner] Found {} working resolvers", new_found.len());

            // Save to cache
            if let Err(e) = super::resolver_cache::save_cache(&self.cache_dir, &new_found) {
                warn!("[bg-scanner] Failed to save cache: {}", e);
            }

            // Notify main thread
            let _ = self.new_resolver_tx.send(new_found);
        }
    }
}

/// Quick startup test: check which resolvers from a list are currently working.
/// Returns only the ones that pass delegation test.
pub async fn quick_startup_test(
    resolvers: &[ResolverConfig],
    domain: &str,
) -> Vec<ResolverConfig> {
    let sem = Arc::new(tokio::sync::Semaphore::new(10));
    let mut tasks = Vec::new();

    for resolver in resolvers {
        let sem = sem.clone();
        let domain = domain.to_string();
        let resolver = resolver.clone();

        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let ip = resolver.ip.first().map(|s| s.as_str()).unwrap_or("");
            let doh = resolver.doh.as_deref();
            let deleg = prober::test_delegation(ip, doh, &domain).await;
            if deleg.works {
                Some(resolver)
            } else {
                None
            }
        }));
    }

    let mut working = Vec::new();
    for task in tasks {
        if let Ok(Some(resolver)) = task.await {
            working.push(resolver);
        }
    }
    working
}

/// Detect network change by comparing current system DNS with previous.
pub async fn detect_network_change(previous_dns: &[String], domain: &str) -> (bool, Vec<String>) {
    let current_dns = detect::read_system_dns();
    let changed = current_dns != previous_dns;
    (changed, current_dns)
}
```

### 5. Update sp-client main.rs

Add new CLI flags:
```rust
#[derive(Parser, Debug)]
struct Args {
    // ... existing fields ...

    /// Enable auto-discovery of resolvers (background scanner)
    #[arg(long, default_value_t = false)]
    auto_discover: bool,

    /// Cache directory for discovered resolvers
    #[arg(long, default_value = ".")]
    cache_dir: PathBuf,

    /// Bootstrap hints (dns:IP, site:URL, doh:URL)
    #[arg(long)]
    hint: Vec<String>,

    /// Skip static config, use only auto-discovered resolvers
    #[arg(long, default_value_t = false)]
    discovery_only: bool,
}
```

Update main() startup sequence:
```rust
#[tokio::main]
async fn main() -> Result<()> {
    // ... existing logging setup ...
    let args = Args::parse();

    // Step 1: Collect resolvers from all sources
    let mut all_resolvers: Vec<sp_common::config::ResolverConfig> = Vec::new();

    // Source A: Static config file (existing behavior)
    if !args.discovery_only {
        if let Ok(config) = Config::load(&args.config) {
            info!("loaded {} resolvers from config", config.resolvers.len());
            all_resolvers.extend(config.resolvers);
        } else {
            warn!("no config file at {:?}", args.config);
        }
    }

    // Source B: Cached resolvers from previous run
    if args.auto_discover {
        if let Some(cached) = resolver_cache::load_cache(&args.cache_dir) {
            info!("loaded {} cached resolvers", cached.resolvers.len());
            all_resolvers.extend(cached.resolvers);
        }
    }

    // Source C: Bootstrap with hints (if provided)
    if !args.hint.is_empty() {
        info!("running bootstrap with {} hints...", args.hint.len());
        // Quick bootstrap: test hints, find working paths
        for hint in &args.hint {
            if let Some(ip) = hint.strip_prefix("dns:") {
                let deleg = sp_scanner::prober::test_delegation(ip, None, &args.domain).await;
                if deleg.works {
                    info!("hint dns:{} — delegation works!", ip);
                    all_resolvers.push(sp_common::config::ResolverConfig {
                        name: format!("hint-{}", ip),
                        ip: vec![ip.to_string()],
                        doh: None,
                        ech: false,
                    });
                }
            } else if let Some(doh_url) = hint.strip_prefix("doh:") {
                let deleg = sp_scanner::prober::test_delegation("", Some(doh_url), &args.domain).await;
                if deleg.works {
                    info!("hint doh:{} — delegation works!", doh_url);
                    all_resolvers.push(sp_common::config::ResolverConfig {
                        name: format!("hint-doh"),
                        ip: vec![],
                        doh: Some(doh_url.to_string()),
                        ech: false,
                    });
                }
            }
            // site: hints require more complex CDN detection — skip for now
        }
    }

    // Source D: If still no resolvers and auto_discover enabled, run quick scan
    if all_resolvers.is_empty() && args.auto_discover {
        warn!("no resolvers from config or cache, running auto-discovery...");
        // Quick scan of bundled resolvers
        let bundled = sp_scanner::resolvers::bundled_resolvers();
        let sem = Arc::new(tokio::sync::Semaphore::new(10));
        let mut tasks = Vec::new();
        for r in &bundled {
            let sem = sem.clone();
            let domain = args.domain.clone();
            let ip = r.ip.to_string();
            let name = r.name.to_string();
            let doh = r.doh_url.map(|s| s.to_string());
            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let deleg = sp_scanner::prober::test_delegation(&ip, doh.as_deref(), &domain).await;
                if deleg.works {
                    Some(sp_common::config::ResolverConfig {
                        name, ip: vec![ip], doh, ech: false,
                    })
                } else {
                    None
                }
            }));
        }
        for task in tasks {
            if let Ok(Some(r)) = task.await {
                all_resolvers.push(r);
            }
        }
        info!("auto-discovered {} working resolvers", all_resolvers.len());
    }

    // Deduplicate by IP
    all_resolvers.sort_by(|a, b| a.ip.cmp(&b.ip));
    all_resolvers.dedup_by(|a, b| a.ip == b.ip);

    if all_resolvers.is_empty() {
        anyhow::bail!("no working resolvers found. Try --hint dns:YOUR_DNS_IP or --hint doh:https://dns.google/dns-query");
    }

    info!("using {} resolvers total", all_resolvers.len());

    // Build config from collected resolvers
    let config = Config { resolvers: all_resolvers.clone() };
    let endpoints = config.all_endpoints();

    // ... rest of existing startup code (create router, add paths, etc.) ...

    // After router is created and tunnel is running:

    // Start background scanner (if enabled)
    if args.auto_discover {
        let (bg_scanner, mut new_resolver_rx) = background_scanner::BackgroundScanner::new(
            args.domain.clone(),
            router.clone(),
            args.cache_dir.clone(),
        );

        // Background scanner task
        tokio::spawn(bg_scanner.run());

        // Hot-swap listener: when background scanner finds new resolvers, add to router
        let router_for_hotswap = router.clone();
        tokio::spawn(async move {
            while new_resolver_rx.changed().await.is_ok() {
                let new_resolvers = new_resolver_rx.borrow().clone();
                for r in &new_resolvers {
                    for ip in &r.ip {
                        // Try to add new paths for this resolver
                        let ep = sp_common::config::ResolverEndpoint {
                            name: r.name.clone(),
                            ip: ip.clone(),
                            doh_url: r.doh.clone(),
                            ech_supported: r.ech,
                        };

                        if let Ok(transport) = sp_transport::dns_udp::DnsUdpTransport::new(ip, &r.name) {
                            router_for_hotswap.add_path(ep.clone(), Arc::new(transport)).await;
                        }
                        if let Some(ref doh_url) = r.doh {
                            if let Ok(transport) = sp_transport::doh::DohTransport::new(doh_url, &r.name) {
                                router_for_hotswap.add_path(ep.clone(), Arc::new(transport)).await;
                            }
                        }
                    }
                }
                info!("[hotswap] Added {} new resolvers to router",
                    new_resolvers.len());
            }
        });

        // Network change detector
        let router_for_netchange = router.clone();
        let domain_for_netchange = args.domain.clone();
        let cache_dir_for_netchange = args.cache_dir.clone();
        tokio::spawn(async move {
            let mut current_dns = detect::read_system_dns();
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let (changed, new_dns) = background_scanner::detect_network_change(
                    &current_dns, &domain_for_netchange
                ).await;
                if changed {
                    warn!("[network] Network change detected! DNS: {:?} → {:?}", current_dns, new_dns);
                    current_dns = new_dns;
                    // TODO: re-run bootstrap, hot-swap resolvers
                    // For now just log the change
                }
            }
        });
    }

    // ... existing SOCKS5 listener code ...
}
```

### 6. Build and test

1. `cargo build` — zero errors (both sp-scanner lib + sp-client)
2. `cargo test -p sp-scanner` — all tests still pass
3. Test client WITHOUT auto-discover (existing behavior unchanged):
   ```
   ./target/debug/sp-client --config config/resolvers.toml --domain tunnel.example.com
   ```
4. Test client WITH auto-discover:
   ```
   ./target/debug/sp-client --auto-discover --domain tunnel.example.com
   ```
5. Test client with hints:
   ```
   ./target/debug/sp-client --auto-discover --hint dns:8.8.8.8 --hint doh:https://dns.google/dns-query --domain tunnel.example.com
   ```
6. Test discovery-only mode (no config file needed):
   ```
   ./target/debug/sp-client --auto-discover --discovery-only --domain tunnel.example.com
   ```
7. Verify background scanner logs appear:
   ```
   [bg-scanner] Background scanner started
   [bg-scanner] Quick test every 300s, full scan every 1800s
   ```
8. Fix any warnings
9. Commit when done

## Constraints
- MUST NOT break existing behavior: `sp-client --config resolvers.toml` works exactly as before
- Auto-discover is opt-in (`--auto-discover` flag)
- sp-scanner becomes both library and binary (both must compile)
- Don't duplicate code — import from sp_scanner crate
- Background tasks must not interfere with tunnel performance
- Hot-swap adds paths to router without restarting tunnel
- Cache file is simple TOML (same format as resolvers.toml)
- Commit when done

## File changes
```
NEW:    crates/sp-scanner/src/lib.rs                  ← library entry point
MODIFY: crates/sp-scanner/Cargo.toml                  ← add [lib] section
MODIFY: crates/sp-scanner/src/main.rs                 ← import from lib instead of mod
MODIFY: crates/sp-client/Cargo.toml                   ← add sp-scanner dependency
NEW:    crates/sp-client/src/resolver_cache.rs         ← cache load/save
NEW:    crates/sp-client/src/background_scanner.rs     ← background discovery task
MODIFY: crates/sp-client/src/main.rs                   ← add auto-discover, hints, cache, bg scanner
```

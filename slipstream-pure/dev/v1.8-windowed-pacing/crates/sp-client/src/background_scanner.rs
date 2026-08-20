use sp_common::config::ResolverConfig;
use sp_multipath::MultipathRouter;
use sp_scanner::prober;
use sp_scanner::resolvers::bundled_resolvers;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{debug, info, warn};

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
            quick_test_interval: 300,  // 5 min
            full_scan_interval: 1800,  // 30 min
            cache_dir,
        };
        (scanner, rx)
    }

    /// Run background scanner. Call this with tokio::spawn.
    pub async fn run(self) {
        info!("[bg-scanner] Background scanner started");
        info!(
            "[bg-scanner] Quick test every {}s, full scan every {}s",
            self.quick_test_interval, self.full_scan_interval
        );

        let mut quick_interval =
            tokio::time::interval(Duration::from_secs(self.quick_test_interval));
        let mut scan_interval =
            tokio::time::interval(Duration::from_secs(self.full_scan_interval));
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

    /// Quick check: send a probe through the router to keep paths alive.
    async fn quick_health_check(&self) {
        let active_count = self.router.active_path_count().await;
        debug!("[bg-scanner] Health check: {} active paths", active_count);

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

        for resolver in &bundled {
            if let Some(doh_url) = resolver.doh_url {
                let deleg =
                    prober::test_delegation(resolver.ip, Some(doh_url), &self.domain).await;

                if deleg.works {
                    debug!(
                        "[bg-scanner] Found working resolver: {} ({})",
                        resolver.name, resolver.ip
                    );
                    new_found.push(ResolverConfig {
                        name: resolver.name.to_string(),
                        ip: vec![resolver.ip.to_string()],
                        doh: resolver.doh_url.map(|s| s.to_string()),
                        ech: false,
                        edns0_max: None,
                        rate_limit_rps: None,
                    });
                }
            }
        }

        if !new_found.is_empty() {
            info!("[bg-scanner] Found {} working resolvers", new_found.len());

            if let Err(e) = super::resolver_cache::save_cache(&self.cache_dir, &new_found) {
                warn!("[bg-scanner] Failed to save cache: {}", e);
            }

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
pub fn detect_network_change(previous_dns: &[String]) -> (bool, Vec<String>) {
    let current_dns = sp_scanner::detect::read_system_dns();
    let changed = current_dns != previous_dns;
    (changed, current_dns)
}

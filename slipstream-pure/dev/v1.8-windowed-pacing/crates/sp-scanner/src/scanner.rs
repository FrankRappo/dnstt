use crate::exclude::is_excluded;
use crate::prober::{self, ProbeMethod};
use crate::resolvers::{self, BundledResolver};
use anyhow::Result;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    Auto,
    Stealth,
    Aggressive,
}

pub struct ScanConfig {
    pub mode: ScanMode,
    pub domain: String,
    pub output: Option<String>,
    /// Force stealth camouflage (decoy traffic, browser-like timing).
    pub camouflage: bool,
    /// Decoy traffic ratio 0.0-1.0.
    pub decoy_ratio: f32,
}

pub struct ScanStats {
    pub total: usize,
    pub scanned: AtomicUsize,
    pub doh_probed: AtomicUsize,
    pub udp_probed: AtomicUsize,
    pub alive: AtomicUsize,
    pub delegation: AtomicUsize,
}

impl ScanStats {
    fn new(total: usize) -> Self {
        Self {
            total,
            scanned: AtomicUsize::new(0),
            doh_probed: AtomicUsize::new(0),
            udp_probed: AtomicUsize::new(0),
            alive: AtomicUsize::new(0),
            delegation: AtomicUsize::new(0),
        }
    }

    fn print_progress(&self) {
        let scanned = self.scanned.load(Ordering::Relaxed);
        let doh = self.doh_probed.load(Ordering::Relaxed);
        let udp = self.udp_probed.load(Ordering::Relaxed);
        let alive = self.alive.load(Ordering::Relaxed);
        let deleg = self.delegation.load(Ordering::Relaxed);
        eprint!(
            "\r[{}/{}] doh={} udp={} alive={} delegation={}    ",
            scanned, self.total, doh, udp, alive, deleg,
        );
    }
}

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

    if report.port53_udp {
        score += 3;
    }
    if report.port53_tcp {
        score += 1;
    }
    if !report.transparent_proxy {
        score += 2;
    }
    if !report.dns_hijack {
        score += 2;
    }
    if report.port853_tls {
        score += 1;
    }
    if !report.captive_portal {
        score += 1;
    }

    let (level, mode) = if score >= 8 {
        (0, ScanMode::Aggressive)
    } else if score >= 5 {
        (1, ScanMode::Stealth)
    } else if score >= 3 {
        (2, ScanMode::Stealth)
    } else {
        (3, ScanMode::Stealth)
    };

    let level_name = match level {
        0 => "OPEN",
        1 => "FILTERED",
        2 => "RESTRICTED",
        _ => "HOSTILE",
    };

    eprintln!(
        "[auto] Network: {} (level {}, score {}/10)",
        level_name, level, score
    );
    eprintln!("[auto] Selected mode: {:?}", mode);
    if level >= 2 {
        eprintln!("[auto] Restricted network — using DoH-only scanning");
    }

    (mode, level, score)
}

/// Run the scan command.
pub async fn run_scan(config: ScanConfig) -> Result<()> {
    let (actual_mode, level, _score) = match config.mode {
        ScanMode::Auto => auto_detect_mode(&config.domain).await,
        other => (other, 0, 10),
    };

    let resolvers = resolvers::bundled_resolvers();
    info!("loaded {} bundled resolvers", resolvers.len());

    // Filter out excluded IPs
    let resolvers: Vec<BundledResolver> = resolvers
        .into_iter()
        .filter(|r| {
            if let Ok(ip) = r.ip.parse::<std::net::Ipv4Addr>() {
                !is_excluded(ip)
            } else {
                true
            }
        })
        .collect();

    // Randomize order
    let mut resolvers = resolvers;
    use rand::seq::SliceRandom;
    let mut rng = rand::thread_rng();
    resolvers.shuffle(&mut rng);

    let stats = Arc::new(ScanStats::new(resolvers.len()));
    let mut alive_ips: Vec<String> = Vec::new();

    // Decide whether to use stealth camouflage:
    // - Explicitly requested via --camouflage
    // - Auto-enabled for RESTRICTED (level 2) or HOSTILE (level 3) networks
    let use_stealth = config.camouflage || (config.mode == ScanMode::Auto && level >= 2);

    if use_stealth {
        // --- Stealth path: sequential scanning with traffic shaping ---
        eprintln!(
            "[stealth] Using traffic camouflage (decoy ratio: {:.0}%, browser-like timing)",
            config.decoy_ratio * 100.0
        );
        let mut stealth = crate::stealth::StealthScanner::new(config.decoy_ratio);

        for resolver in &resolvers {
            let doh_url = if actual_mode == ScanMode::Aggressive {
                None
            } else {
                resolver.doh_url
            };

            let result = stealth
                .stealth_probe(resolver.ip, doh_url, &config.domain)
                .await;

            let scanned = stats.scanned.fetch_add(1, Ordering::Relaxed) + 1;
            match result.method {
                ProbeMethod::Doh => {
                    stats.doh_probed.fetch_add(1, Ordering::Relaxed);
                }
                ProbeMethod::Udp => {
                    stats.udp_probed.fetch_add(1, Ordering::Relaxed);
                }
            }
            if result.alive {
                stats.alive.fetch_add(1, Ordering::Relaxed);
                alive_ips.push(result.ip.clone());
            }

            let alive = stats.alive.load(Ordering::Relaxed);
            let avg_delay = stealth.avg_delay_ms();
            eprint!(
                "\r[{}/{}] probed={} alive={} decoys={} avg_delay={}ms    ",
                scanned, stats.total, scanned, alive, stealth.decoy_count, avg_delay,
            );
        }
    } else {
        // --- Normal path: parallel scanning (unchanged) ---
        let (concurrency_doh, concurrency_udp) = match actual_mode {
            ScanMode::Auto => unreachable!(),
            ScanMode::Stealth => (10, 2),
            ScanMode::Aggressive => (50, 50),
        };

        let sem_doh = Arc::new(Semaphore::new(concurrency_doh));
        let sem_udp = Arc::new(Semaphore::new(concurrency_udp));

        let mut handles = Vec::new();

        for resolver in resolvers {
            let domain = config.domain.clone();
            let stats = stats.clone();
            let sem_doh = sem_doh.clone();
            let sem_udp = sem_udp.clone();
            let mode = actual_mode;

            let handle = tokio::spawn(async move {
                let has_doh = resolver.doh_url.is_some() && mode == ScanMode::Stealth;
                let _permit = if has_doh {
                    sem_doh.acquire().await.unwrap()
                } else {
                    sem_udp.acquire().await.unwrap()
                };

                let doh_url = if mode == ScanMode::Stealth {
                    resolver.doh_url
                } else {
                    None
                };

                let result = prober::probe_resolver(resolver.ip, doh_url, &domain).await;

                stats.scanned.fetch_add(1, Ordering::Relaxed);
                match result.method {
                    ProbeMethod::Doh => {
                        stats.doh_probed.fetch_add(1, Ordering::Relaxed);
                    }
                    ProbeMethod::Udp => {
                        stats.udp_probed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if result.alive {
                    stats.alive.fetch_add(1, Ordering::Relaxed);
                }

                stats.print_progress();
                result
            });
            handles.push(handle);
        }

        for handle in handles {
            match handle.await {
                Ok(result) => {
                    if result.alive {
                        alive_ips.push(result.ip.clone());
                    }
                }
                Err(e) => warn!("task error: {}", e),
            }
        }
    }

    eprintln!(); // newline after progress
    info!(
        "scan complete: {}/{} alive",
        alive_ips.len(),
        stats.total,
    );

    // Write output
    let output_path = config.output.unwrap_or_else(|| "alive.txt".to_string());
    let content = alive_ips.join("\n");
    tokio::fs::write(&output_path, content).await?;
    info!("wrote {} alive resolvers to {}", alive_ips.len(), output_path);

    Ok(())
}

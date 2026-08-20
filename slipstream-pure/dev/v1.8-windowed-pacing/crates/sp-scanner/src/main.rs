mod bootstrap;
mod detect;
mod exclude;
mod hints;
mod monitor;
mod prober;
mod profile;
mod resolvers;
mod scan_doh;
mod scan_doq;
mod scan_dot;
mod scanner;
#[allow(dead_code)]
mod stealth;
mod test_cmd;
#[allow(dead_code)]
mod timing_stego;
mod validate;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dns-scanner", about = "DNS resolver discovery tool for Slipstream Pure")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Tunnel domain
    #[arg(long, global = true, default_value = "tunnel.example.com")]
    domain: String,

    /// Output file
    #[arg(long, global = true)]
    output: Option<String>,

    /// Verbose logging
    #[arg(long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Discover DNS resolvers (auto-detects network, or --mode stealth/aggressive)
    Scan {
        /// Scan mode: auto (default), stealth, or aggressive
        #[arg(long, default_value = "auto")]
        mode: String,

        /// Force stealth camouflage (decoy traffic, browser-like timing)
        #[arg(long)]
        camouflage: bool,

        /// Decoy traffic ratio 0.0-1.0 (default 0.3 = 30% decoy queries)
        #[arg(long, default_value = "0.3")]
        decoy_ratio: f32,
    },
    /// Test delegation and record types for found resolvers
    Validate {
        /// Input file with alive IPs (one per line)
        #[arg(default_value = "alive.txt")]
        input: String,
    },
    /// Quick reachability test for resolver list
    Test {
        /// Input resolver list (TOML format)
        #[arg(default_value = "config/resolvers.toml")]
        input: String,
    },
    /// Auto-discover working DNS path (for restricted networks)
    Bootstrap {
        /// Hints: dns:IP, site:URL, doh:URL (can specify multiple)
        #[arg(long, num_args = 1..)]
        hint: Vec<String>,
    },
    /// Help user find hints for bootstrap
    Hints,
    /// Analyze network environment
    Detect,
    /// Deep resolver analysis (rate limit, fingerprint, honeypot detection)
    Profile {
        /// Input resolver list (alive.txt or resolvers.toml)
        #[arg(default_value = "alive.txt")]
        input: String,
    },
    /// Continuous monitoring of resolvers
    Monitor {
        /// Input resolver list
        #[arg(default_value = "alive.txt")]
        input: String,

        /// Check interval in seconds
        #[arg(long, default_value = "600")]
        interval: u64,

        /// Path to history JSON file (persistent across restarts)
        #[arg(long, default_value = "monitor-history.json")]
        history: String,
    },
    /// Discover new DoH endpoints (CT logs, subdomain brute-force)
    ScanDoh,
    /// Discover DoT endpoints on port 853
    ScanDot,
    /// Discover DoQ endpoints (experimental)
    ScanDoq,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install rustls crypto provider (needed for DoT/TLS)
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();

    let log_level = if cli.verbose {
        "debug,sp_scanner=trace"
    } else {
        "info,sp_scanner=debug"
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_level.parse().unwrap()),
        )
        .init();

    match cli.command {
        Commands::Scan {
            mode,
            camouflage,
            decoy_ratio,
        } => {
            let scan_mode = match mode.as_str() {
                "aggressive" => scanner::ScanMode::Aggressive,
                "stealth" => scanner::ScanMode::Stealth,
                _ => scanner::ScanMode::Auto,
            };
            scanner::run_scan(scanner::ScanConfig {
                mode: scan_mode,
                domain: cli.domain,
                output: cli.output,
                camouflage,
                decoy_ratio,
            })
            .await?;
        }
        Commands::Validate { input } => {
            validate::run_validate(validate::ValidateConfig {
                input,
                domain: cli.domain,
                output: cli.output,
            })
            .await?;
        }
        Commands::Test { input } => {
            test_cmd::run_test(test_cmd::TestConfig {
                input,
                domain: cli.domain,
                output: cli.output,
            })
            .await?;
        }
        Commands::Bootstrap { hint } => {
            bootstrap::run_bootstrap(bootstrap::BootstrapConfig {
                hints: hint,
                domain: cli.domain,
                output: cli.output,
            })
            .await?;
        }
        Commands::Hints => hints::run_hints(&cli.domain).await?,
        Commands::Detect => detect::run_detect(&cli.domain).await?,
        Commands::Profile { input } => {
            profile::run_profile(profile::ProfileConfig {
                input,
                domain: cli.domain,
                output: cli.output,
            })
            .await?;
        }
        Commands::Monitor {
            input,
            interval,
            history,
        } => {
            monitor::run_monitor(monitor::MonitorConfig {
                input,
                domain: cli.domain,
                interval_secs: interval,
                history_path: history,
            })
            .await?;
        }
        Commands::ScanDoh => {
            scan_doh::run_scan_doh(scan_doh::ScanDohConfig {
                domain: cli.domain,
                output: cli.output,
            })
            .await?;
        }
        Commands::ScanDot => {
            scan_dot::run_scan_dot(scan_dot::ScanDotConfig {
                domain: cli.domain,
                output: cli.output,
            })
            .await?;
        }
        Commands::ScanDoq => {
            scan_doq::run_scan_doq(scan_doq::ScanDoqConfig {
                domain: cli.domain,
                output: cli.output,
            })
            .await?;
        }
    }

    Ok(())
}

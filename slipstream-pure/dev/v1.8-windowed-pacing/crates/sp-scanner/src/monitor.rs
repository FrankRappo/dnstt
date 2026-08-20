// Phase 3+5: Continuous resolver monitoring with reliability tracking and persistent history.

use anyhow::Result;
use std::collections::HashMap;
use std::time::Duration;

use crate::prober;

pub struct MonitorConfig {
    pub input: String,
    pub domain: String,
    pub interval_secs: u64,
    pub history_path: String,
}

#[derive(Debug, Clone)]
struct CheckResult {
    timestamp: u64,
    alive: bool,
    delegation: bool,
    latency_ms: Option<u64>,
}

pub async fn run_monitor(config: MonitorConfig) -> Result<()> {
    let ips = load_resolver_list(&config.input)?;
    if ips.is_empty() {
        anyhow::bail!("no resolvers found in {}", config.input);
    }
    eprintln!(
        "Monitoring {} resolvers every {} seconds",
        ips.len(),
        config.interval_secs
    );
    eprintln!("History file: {}", config.history_path);
    eprintln!("Press Ctrl+C to stop\n");

    // Load persistent history
    let mut history = load_history(&config.history_path);
    if !history.is_empty() {
        let total_checks: usize = history.values().map(|v| v.len()).sum();
        eprintln!(
            "Loaded {} previous checks for {} resolvers\n",
            total_checks,
            history.len()
        );
    }

    loop {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        eprintln!("=== Check at {} ===", timestamp);

        for ip in &ips {
            let probe = prober::probe_resolver(ip, None, &config.domain).await;
            let deleg = if probe.alive {
                let d = prober::test_delegation(ip, None, &config.domain).await;
                d.works
            } else {
                false
            };

            let result = CheckResult {
                timestamp,
                alive: probe.alive,
                delegation: deleg,
                latency_ms: probe.latency_ms,
            };

            let entry = history.entry(ip.clone()).or_default();
            entry.push(result);

            let total = entry.len();
            let alive_count = entry.iter().filter(|r| r.alive).count();
            let deleg_count = entry.iter().filter(|r| r.delegation).count();
            let uptime_pct = (alive_count as f64 / total as f64) * 100.0;
            let deleg_pct = (deleg_count as f64 / total as f64) * 100.0;
            let reliability = calculate_reliability(entry);

            let status = if deleg {
                "OK"
            } else if probe.alive {
                "NO-DELEG"
            } else {
                "DOWN"
            };
            let latency_str = probe
                .latency_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "---".to_string());

            eprintln!(
                "  {} {:>7} {:>6}  uptime={:.0}% deleg={:.0}% reliability={}% ({} checks)",
                ip, status, latency_str, uptime_pct, deleg_pct, reliability, total
            );

            // Alert on state change
            if total >= 2 {
                let prev = &entry[total - 2];
                if prev.delegation && !deleg {
                    eprintln!("  *** ALERT: {} delegation LOST! ***", ip);
                }
                if !prev.alive && probe.alive {
                    eprintln!("  *** ALERT: {} came BACK UP ***", ip);
                }
            }
        }

        // Save history after each check cycle
        if let Err(e) = save_history(&history, &config.history_path) {
            eprintln!("  Warning: failed to save history: {}", e);
        }

        // Use adaptive interval based on worst reliability
        let min_reliability = ips
            .iter()
            .filter_map(|ip| history.get(ip))
            .map(|h| calculate_reliability(h))
            .min()
            .unwrap_or(0);
        let sleep_secs = adaptive_interval(config.interval_secs, min_reliability);
        if sleep_secs != config.interval_secs {
            eprintln!(
                "  (adaptive interval: {}s based on reliability {}%)",
                sleep_secs, min_reliability
            );
        }

        eprintln!();
        tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
    }
}

fn calculate_reliability(history: &[CheckResult]) -> u8 {
    if history.is_empty() {
        return 0;
    }
    let deleg_ok = history.iter().filter(|r| r.delegation).count();
    ((deleg_ok as f64 / history.len() as f64) * 100.0) as u8
}

fn adaptive_interval(base_interval: u64, reliability: u8) -> u64 {
    if reliability >= 90 {
        base_interval * 2 // stable — check less often
    } else if reliability >= 50 {
        base_interval // normal
    } else {
        (base_interval / 2).max(10) // unstable — check more often (min 10s)
    }
}

fn save_history(history: &HashMap<String, Vec<CheckResult>>, path: &str) -> Result<()> {
    let mut map = serde_json::Map::new();
    for (ip, checks) in history {
        let arr: Vec<serde_json::Value> = checks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "ts": c.timestamp,
                    "alive": c.alive,
                    "deleg": c.delegation,
                    "latency": c.latency_ms,
                })
            })
            .collect();
        map.insert(ip.clone(), serde_json::Value::Array(arr));
    }
    let json = serde_json::to_string_pretty(&serde_json::Value::Object(map))?;
    std::fs::write(path, json)?;
    Ok(())
}

fn load_history(path: &str) -> HashMap<String, Vec<CheckResult>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };
    let mut history = HashMap::new();
    if let Some(obj) = parsed.as_object() {
        for (ip, arr) in obj {
            if let Some(checks) = arr.as_array() {
                let results: Vec<CheckResult> = checks
                    .iter()
                    .filter_map(|v| {
                        Some(CheckResult {
                            timestamp: v.get("ts")?.as_u64()?,
                            alive: v.get("alive")?.as_bool()?,
                            delegation: v.get("deleg")?.as_bool()?,
                            latency_ms: v.get("latency").and_then(|l| l.as_u64()),
                        })
                    })
                    .collect();
                history.insert(ip.clone(), results);
            }
        }
    }
    history
}

fn load_resolver_list(path: &str) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    if path.ends_with(".toml") {
        let config: toml::Value = toml::from_str(&content)?;
        let mut ips = Vec::new();
        if let Some(resolvers) = config.get("resolver").and_then(|v| v.as_array()) {
            for r in resolvers {
                if let Some(ip_arr) = r.get("ip").and_then(|v| v.as_array()) {
                    for ip in ip_arr {
                        if let Some(s) = ip.as_str() {
                            ips.push(s.to_string());
                        }
                    }
                }
            }
        }
        Ok(ips)
    } else {
        Ok(content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect())
    }
}

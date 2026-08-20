use crate::prober;
use crate::resolvers;
use anyhow::Result;
use hickory_proto::rr::RecordType;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

/// Validation result for a single resolver.
#[derive(Debug, Clone)]
pub struct ValidatedResolver {
    pub ip: String,
    pub name: String,
    pub doh_url: Option<String>,
    pub delegation_works: bool,
    pub real_resolver_ip: Option<String>,
    pub latency_ms: u64,
    pub edns0_max: u16,
    pub supported_types: Vec<String>,
    pub best_type: String,
}

pub struct ValidateConfig {
    pub input: String,
    pub domain: String,
    pub output: Option<String>,
}

/// Run the validate command.
pub async fn run_validate(config: ValidateConfig) -> Result<()> {
    // Read alive IPs
    let content = tokio::fs::read_to_string(&config.input).await?;
    let ips: Vec<String> = content
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    info!("validating {} resolvers from {}", ips.len(), config.input);

    // Build a map of known resolvers for DoH URLs and names
    let bundled = resolvers::bundled_resolvers();
    let lookup: std::collections::HashMap<&str, (&str, Option<&str>)> = bundled
        .iter()
        .map(|r| (r.ip, (r.name, r.doh_url)))
        .collect();

    let sem = Arc::new(Semaphore::new(10));
    let total = ips.len();
    let done = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();

    for ip in ips {
        let domain = config.domain.clone();
        let sem = sem.clone();
        let done = done.clone();
        let (name, doh_url) = lookup
            .get(ip.as_str())
            .map(|(n, d)| (n.to_string(), d.map(|s| s.to_string())))
            .unwrap_or_else(|| (ip.clone(), None));

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            let result = validate_resolver(&ip, &name, doh_url.as_deref(), &domain).await;

            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            eprint!("\r[{}/{}] validating...    ", d, total);

            result
        });
        handles.push(handle);
    }

    let mut validated = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(Ok(v)) => validated.push(v),
            Ok(Err(e)) => debug!("validation error: {}", e),
            Err(e) => warn!("task error: {}", e),
        }
    }

    eprintln!();

    // Sort by latency
    validated.sort_by_key(|v| v.latency_ms);

    // Only keep resolvers where delegation works
    let working: Vec<&ValidatedResolver> = validated.iter().filter(|v| v.delegation_works).collect();
    info!(
        "validation complete: {}/{} with working delegation",
        working.len(),
        validated.len()
    );

    // Write TOML output
    let output_path = config
        .output
        .unwrap_or_else(|| "resolvers-validated.toml".to_string());
    let toml_content = format_validated_toml(&validated);
    tokio::fs::write(&output_path, toml_content).await?;
    info!("wrote {} resolvers to {}", validated.len(), output_path);

    Ok(())
}

async fn validate_resolver(
    ip: &str,
    name: &str,
    doh_url: Option<&str>,
    domain: &str,
) -> Result<ValidatedResolver> {
    // 1. Delegation test (3 probes, average latency)
    let mut latencies = Vec::new();
    let mut real_ip = None;
    let mut delegation_works = false;

    for _ in 0..3 {
        let result = prober::test_delegation(ip, doh_url, domain).await;
        if result.works {
            delegation_works = true;
            if let Some(rip) = &result.real_resolver_ip {
                real_ip = Some(rip.clone());
            }
        }
        if let Some(lat) = result.latency_ms {
            latencies.push(lat);
        }
    }

    let avg_latency = if latencies.is_empty() {
        0
    } else {
        latencies.iter().sum::<u64>() / latencies.len() as u64
    };

    // 2. Test record types
    let test_types = [
        (RecordType::TXT, "TXT"),
        (RecordType::A, "A"),
        (RecordType::AAAA, "AAAA"),
        (RecordType::HTTPS, "HTTPS"),
        (RecordType::DNSKEY, "DNSKEY"),
        (RecordType::Unknown(10), "NULL"), // type 10 = NULL
    ];

    let mut supported = Vec::new();
    for (rtype, rtype_name) in &test_types {
        let probe_name = format!("probe-{:08x}.{}", rand::random::<u32>(), domain);
        if test_record_type(ip, doh_url, &probe_name, *rtype).await {
            supported.push(rtype_name.to_string());
        }
    }

    let best_type = if supported.contains(&"HTTPS".to_string()) {
        "HTTPS".to_string()
    } else if supported.contains(&"TXT".to_string()) {
        "TXT".to_string()
    } else {
        supported.first().cloned().unwrap_or_else(|| "TXT".to_string())
    };

    // 3. EDNS0 size test
    let mut edns0_max: u16 = 512;
    for size in [512u16, 1232, 4096] {
        let probe_name = format!("probe-{:08x}.{}", rand::random::<u32>(), domain);
        if test_edns0_size(ip, doh_url, &probe_name, size).await {
            edns0_max = size;
        }
    }

    Ok(ValidatedResolver {
        ip: ip.to_string(),
        name: name.to_string(),
        doh_url: doh_url.map(|s| s.to_string()),
        delegation_works,
        real_resolver_ip: real_ip,
        latency_ms: avg_latency,
        edns0_max,
        supported_types: supported,
        best_type,
    })
}

/// Test if a resolver supports a specific record type.
async fn test_record_type(
    ip: &str,
    doh_url: Option<&str>,
    qname: &str,
    rtype: RecordType,
) -> bool {
    let query = match crate::prober::build_dns_typed_query(qname, rtype) {
        Ok(q) => q,
        Err(_) => return false,
    };

    // Try DoH first
    if let Some(url) = doh_url {
        if let Ok(true) = send_doh_check(url, &query).await {
            return true;
        }
    }

    // Fallback UDP
    send_udp_check(ip, &query).await.unwrap_or(false)
}

/// Test EDNS0 buffer size support.
async fn test_edns0_size(
    ip: &str,
    doh_url: Option<&str>,
    qname: &str,
    size: u16,
) -> bool {
    let query = match crate::prober::build_dns_query_with_edns_size(qname, size) {
        Ok(q) => q,
        Err(_) => return false,
    };

    if let Some(url) = doh_url {
        if let Ok(true) = send_doh_check(url, &query).await {
            return true;
        }
    }

    send_udp_check(ip, &query).await.unwrap_or(false)
}

async fn send_doh_check(doh_url: &str, query: &sp_transport::DnsQuery) -> Result<bool> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let resp = client
        .post(doh_url)
        .header("Content-Type", "application/dns-message")
        .header("Accept", "application/dns-message")
        .body(query.wire.to_vec())
        .send()
        .await?;

    Ok(resp.status().is_success())
}

async fn send_udp_check(ip: &str, query: &sp_transport::DnsQuery) -> Result<bool> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let addr = format!("{}:53", ip);
    socket.connect(&addr).await?;
    socket.send(&query.wire).await?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("timeout"))??;

    // Valid response with no error = supported
    use hickory_proto::op::Message;
    use hickory_proto::serialize::binary::BinDecodable;
    let msg = Message::from_bytes(&buf[..n])?;
    Ok(!msg.answers().is_empty() || msg.response_code() == hickory_proto::op::ResponseCode::NoError)
}

fn format_validated_toml(resolvers: &[ValidatedResolver]) -> String {
    let mut out = String::from("# Validated DNS resolvers — generated by dns-scanner\n\n");

    for r in resolvers {
        out.push_str("[[resolver]]\n");
        out.push_str(&format!("name = \"{}\"\n", r.name));
        out.push_str(&format!("ip = [\"{}\"]\n", r.ip));
        if let Some(ref doh) = r.doh_url {
            out.push_str(&format!("doh = \"{}\"\n", doh));
        }
        out.push_str(&format!("latency_ms = {}\n", r.latency_ms));
        out.push_str(&format!("edns0_max = {}\n", r.edns0_max));
        let types_str: Vec<String> = r.supported_types.iter().map(|t| format!("\"{}\"", t)).collect();
        out.push_str(&format!("supported_types = [{}]\n", types_str.join(", ")));
        out.push_str(&format!("best_type = \"{}\"\n", r.best_type));
        out.push_str(&format!("delegation = {}\n", r.delegation_works));
        if let Some(ref rip) = r.real_resolver_ip {
            out.push_str(&format!("real_resolver_ip = \"{}\"\n", rip));
        }
        out.push('\n');
    }

    out
}

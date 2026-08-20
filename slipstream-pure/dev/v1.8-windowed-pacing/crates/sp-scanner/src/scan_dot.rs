// Phase 5: DoT (DNS-over-TLS) endpoint discovery — port 853.

use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub struct ScanDotConfig {
    pub domain: String,
    pub output: Option<String>,
}

pub async fn run_scan_dot(config: ScanDotConfig) -> Result<()> {
    let known_dot: Vec<(&str, &str)> = vec![
        ("1.1.1.1", "Cloudflare"),
        ("1.0.0.1", "Cloudflare"),
        ("8.8.8.8", "Google"),
        ("8.8.4.4", "Google"),
        ("9.9.9.9", "Quad9"),
        ("149.112.112.112", "Quad9"),
        ("94.140.14.14", "AdGuard"),
        ("94.140.15.15", "AdGuard"),
        ("185.228.168.9", "CleanBrowsing"),
        ("45.90.28.0", "NextDNS"),
        ("194.242.2.2", "Mullvad"),
        ("185.222.222.222", "DNS.SB"),
        ("45.11.45.11", "DNS.SB"),
        ("208.67.222.222", "OpenDNS"),
        ("208.67.220.220", "OpenDNS"),
        ("76.76.2.0", "ControlD"),
    ];

    eprintln!(
        "Testing {} known DoT endpoints on port 853...\n",
        known_dot.len()
    );

    let sem = Arc::new(Semaphore::new(10));
    let mut tasks = Vec::new();

    for (ip, name) in &known_dot {
        let sem = sem.clone();
        let domain = config.domain.clone();
        let ip = ip.to_string();
        let name = name.to_string();

        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let result = test_dot_endpoint(&ip, &domain).await;
            (ip, name, result)
        }));
    }

    let mut working = Vec::new();
    for task in tasks {
        match task.await {
            Ok((ip, name, Ok(Some((latency, delegation))))) => {
                eprintln!(
                    "  + {} ({}) — {}ms, delegation={}",
                    ip, name, latency, delegation
                );
                working.push((ip, name, latency, delegation));
            }
            Ok((ip, name, _)) => {
                eprintln!("  - {} ({}) — not available", ip, name);
            }
            Err(e) => {
                tracing::debug!("DoT task join error: {}", e);
            }
        }
    }

    eprintln!("\n=== Found {} working DoT endpoints ===", working.len());

    if let Some(path) = &config.output {
        write_dot_results(&working, path)?;
    }

    Ok(())
}

async fn test_dot_endpoint(ip: &str, domain: &str) -> Result<Option<(u64, bool)>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoCertVerifier))
        .with_no_client_auth();

    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));

    let addr: std::net::SocketAddr = format!("{}:853", ip).parse()?;
    let tcp = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
    .await??;

    let server_name = rustls::pki_types::ServerName::try_from(ip.to_string())
        .unwrap_or_else(|_| {
            rustls::pki_types::ServerName::try_from("dns.example.com".to_string()).unwrap()
        });

    let mut tls = tokio::time::timeout(
        Duration::from_secs(5),
        connector.connect(server_name, tcp),
    )
    .await??;

    // DNS-over-TLS: 2-byte length prefix + DNS wire format
    let probe_name = format!("probe-{:08x}.{}", rand::random::<u32>(), domain);
    let query = sp_transport::build_dns_txt_query(&probe_name)?;
    let wire = query.wire.to_vec();
    let len = (wire.len() as u16).to_be_bytes();

    let start = Instant::now();

    tls.write_all(&len).await?;
    tls.write_all(&wire).await?;
    tls.flush().await?;

    // Read response: 2-byte length prefix + DNS wire
    let mut len_buf = [0u8; 2];
    tokio::time::timeout(Duration::from_secs(5), tls.read_exact(&mut len_buf)).await??;
    let resp_len = u16::from_be_bytes(len_buf) as usize;

    if resp_len > 65535 {
        anyhow::bail!("response too large: {}", resp_len);
    }

    let mut resp_buf = vec![0u8; resp_len];
    tokio::time::timeout(Duration::from_secs(5), tls.read_exact(&mut resp_buf)).await??;

    let latency = start.elapsed().as_millis() as u64;

    let parsed = sp_transport::parse_dns_response(&resp_buf)?;
    let delegation = parsed.txt_data.is_some();

    Ok(Some((latency, delegation)))
}

// Simple cert verifier that accepts anything (for testing/discovery)
#[derive(Debug)]
struct NoCertVerifier;

impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
    {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
    {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn write_dot_results(
    working: &[(String, String, u64, bool)],
    path: &str,
) -> Result<()> {
    let mut content = String::from("# Discovered DoT endpoints\n\n");
    for (ip, name, latency, delegation) in working {
        content.push_str("[[endpoint]]\n");
        content.push_str(&format!("ip = \"{}\"\n", ip));
        content.push_str(&format!("name = \"{}\"\n", name));
        content.push_str(&format!("latency_ms = {}\n", latency));
        content.push_str(&format!("delegation_works = {}\n\n", delegation));
    }
    std::fs::write(path, content)?;
    Ok(())
}

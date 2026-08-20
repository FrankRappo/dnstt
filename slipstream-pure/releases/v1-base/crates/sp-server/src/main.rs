use anyhow::Result;
use bytes::Bytes;
use clap::Parser;
use quinn::Endpoint;
use sp_common::crypto;
use sp_common::dns_codec;
use sp_common::protocol::{PacketType, TunnelPacket};
use sp_transport::{build_dns_response, parse_dns_response};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "sp-server", about = "Slipstream Pure DNS Tunnel Server")]
struct Args {
    /// Domain to listen for (authoritative zone)
    #[arg(long, default_value = "tunnel.example.com")]
    domain: String,

    /// Listen address for DNS (UDP + TCP)
    #[arg(long, default_value = "0.0.0.0:53")]
    dns_listen: String,

    /// SOCKS5 proxy address to forward traffic to
    #[arg(long, default_value = "127.0.0.1:1080")]
    socks_addr: String,

    /// TLS certificate PEM file (generated if not exists)
    #[arg(long, default_value = "server.crt")]
    cert: PathBuf,

    /// TLS private key PEM file (generated if not exists)
    #[arg(long, default_value = "server.key")]
    key: PathBuf,
}

/// Per-session state on the server side
struct Session {
    id: String,
    /// Downstream data waiting to be sent back via DNS responses
    downstream_queue: tokio::sync::Mutex<Vec<Bytes>>,
    /// Connection to SOCKS5 proxy
    socks_tx: tokio::sync::mpsc::Sender<Bytes>,
}

type Sessions = Arc<RwLock<std::collections::HashMap<String, Arc<Session>>>>;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sp_server=debug".parse().unwrap()),
        )
        .init();

    let args = Args::parse();
    info!("Slipstream Pure Server starting");
    info!("domain: {}", args.domain);
    info!("DNS listen: {}", args.dns_listen);
    info!("SOCKS5 forward: {}", args.socks_addr);

    // Load or generate TLS certificate
    let (certs, key) = if args.cert.exists() && args.key.exists() {
        info!("loading TLS cert from {:?}", args.cert);
        let certs = crypto::load_cert_pem(&args.cert)?;
        let key = crypto::load_key_pem(&args.key)?;
        (certs, key)
    } else {
        info!("generating self-signed TLS cert for {}", args.domain);
        let (certs, key) = crypto::save_cert_pem(&args.cert, &args.key, &args.domain)?;
        info!("saved cert to {:?}, key to {:?}", args.cert, args.key);
        (certs, key)
    };

    let sessions: Sessions = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let dns_addr: SocketAddr = args.dns_listen.parse()?;

    // Start DNS UDP listener
    let udp_socket = UdpSocket::bind(dns_addr).await?;
    info!("DNS UDP listening on {}", dns_addr);

    let domain = Arc::new(args.domain.clone());
    let socks_addr = Arc::new(args.socks_addr.clone());

    // DNS UDP handler loop
    let sessions_udp = sessions.clone();
    let domain_udp = domain.clone();
    let socks_addr_udp = socks_addr.clone();
    let udp_socket = Arc::new(udp_socket);

    let udp_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            match udp_socket.recv_from(&mut buf).await {
                Ok((n, src)) => {
                    let data = buf[..n].to_vec();
                    let sessions = sessions_udp.clone();
                    let domain = domain_udp.clone();
                    let socks_addr = socks_addr_udp.clone();
                    let udp = udp_socket.clone();
                    tokio::spawn(async move {
                        match handle_dns_query(&data, &domain, &socks_addr, &sessions).await {
                            Ok(response) => {
                                if let Err(e) = udp.send_to(&response, src).await {
                                    error!("UDP send error to {}: {}", src, e);
                                }
                            }
                            Err(e) => {
                                debug!("DNS query handling error from {}: {}", src, e);
                            }
                        }
                    });
                }
                Err(e) => {
                    error!("UDP recv error: {}", e);
                }
            }
        }
    });

    // DNS TCP listener
    let tcp_listener = tokio::net::TcpListener::bind(dns_addr).await?;
    info!("DNS TCP listening on {}", dns_addr);

    let sessions_tcp = sessions.clone();
    let domain_tcp = domain.clone();
    let socks_addr_tcp = socks_addr.clone();

    let tcp_handle = tokio::spawn(async move {
        loop {
            match tcp_listener.accept().await {
                Ok((stream, src)) => {
                    let sessions = sessions_tcp.clone();
                    let domain = domain_tcp.clone();
                    let socks_addr = socks_addr_tcp.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            handle_tcp_client(stream, src, &domain, &socks_addr, &sessions).await
                        {
                            debug!("TCP client {} error: {}", src, e);
                        }
                    });
                }
                Err(e) => {
                    error!("TCP accept error: {}", e);
                }
            }
        }
    });

    info!("Server running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    Ok(())
}

/// Handle a single DNS TCP client connection.
async fn handle_tcp_client(
    mut stream: tokio::net::TcpStream,
    src: SocketAddr,
    domain: &str,
    socks_addr: &str,
    sessions: &Sessions,
) -> Result<()> {
    loop {
        // Read 2-byte length prefix
        let mut len_buf = [0u8; 2];
        if stream.read_exact(&mut len_buf).await.is_err() {
            return Ok(()); // Connection closed
        }
        let len = u16::from_be_bytes(len_buf) as usize;
        if len == 0 || len > 65535 {
            return Ok(());
        }

        let mut query_buf = vec![0u8; len];
        stream.read_exact(&mut query_buf).await?;

        match handle_dns_query(&query_buf, domain, socks_addr, sessions).await {
            Ok(response) => {
                let resp_len = response.len() as u16;
                stream.write_all(&resp_len.to_be_bytes()).await?;
                stream.write_all(&response).await?;
                stream.flush().await?;
            }
            Err(e) => {
                debug!("DNS query error from TCP {}: {}", src, e);
            }
        }
    }
}

/// Process a DNS query: extract tunnel data, handle session, return DNS response.
async fn handle_dns_query(
    query_wire: &[u8],
    domain: &str,
    socks_addr: &str,
    sessions: &Sessions,
) -> Result<Vec<u8>> {
    use hickory_proto::op::Message;
    use hickory_proto::serialize::binary::BinDecodable;

    let message = Message::from_bytes(query_wire)?;
    let query = message
        .queries()
        .first()
        .ok_or_else(|| anyhow::anyhow!("no query in DNS message"))?;

    let qname = query.name().to_string();
    let qname_lower = qname.to_lowercase();

    // Check if this is a query for our domain
    if !qname_lower.ends_with(&format!(".{}.", domain))
        && !qname_lower.ends_with(&format!(".{}", domain))
    {
        // Not our domain — return REFUSED
        return build_refused_response(query_wire);
    }

    // Parse: <data-labels>.<session-id>.<domain>
    // The session-id is the label just before the domain
    let domain_suffix = format!(".{}", domain);
    let without_domain = qname_lower
        .trim_end_matches('.')
        .strip_suffix(&domain_suffix)
        .or_else(|| qname_lower.trim_end_matches('.').strip_suffix(domain))
        .ok_or_else(|| anyhow::anyhow!("cannot strip domain from {}", qname))?;

    // Split into parts; last part is session_id
    let parts: Vec<&str> = without_domain.split('.').collect();
    if parts.is_empty() {
        return build_refused_response(query_wire);
    }

    let session_id = parts.last().unwrap().to_string();
    let data_labels = &parts[..parts.len() - 1];

    // Decode the upstream data
    let upstream_data = if data_labels.is_empty() {
        // Just a poll (no data)
        Vec::new()
    } else {
        let encoded_name = format!(
            "{}.{}.{}",
            data_labels.join("."),
            session_id,
            domain
        );
        dns_codec::decode_upstream(&encoded_name, &session_id, domain)
            .unwrap_or_default()
    };

    // Get or create session
    let session = {
        let sessions_read = sessions.read().await;
        sessions_read.get(&session_id).cloned()
    };

    let session = match session {
        Some(s) => s,
        None => {
            // New session — create it
            info!("new session: {}", session_id);
            let (socks_tx, socks_rx) = tokio::sync::mpsc::channel(256);
            let session = Arc::new(Session {
                id: session_id.clone(),
                downstream_queue: tokio::sync::Mutex::new(Vec::new()),
                socks_tx,
            });
            sessions.write().await.insert(session_id.clone(), session.clone());

            // Spawn SOCKS5 forwarding task
            let session_weak = Arc::downgrade(&session);
            let socks_addr_owned = socks_addr.to_string();
            let sessions_ref = sessions.clone();
            let sid = session_id.clone();
            tokio::spawn(async move {
                if let Err(e) = socks_forward_task(
                    socks_rx,
                    session_weak,
                    &socks_addr_owned,
                ).await {
                    warn!("SOCKS forward task for {} ended: {}", sid, e);
                }
                // Clean up session on exit
                sessions_ref.write().await.remove(&sid);
            });

            session
        }
    };

    // Process upstream data
    if !upstream_data.is_empty() {
        // Decompress
        let decompressed = sp_common::compress::decompress(&upstream_data)
            .unwrap_or_else(|_| upstream_data.clone());

        // Decode tunnel packet
        if let Ok(packet) = TunnelPacket::decode(&decompressed) {
            match packet.packet_type {
                PacketType::Data => {
                    let _ = session.socks_tx.send(packet.payload).await;
                }
                PacketType::Poll => {
                    // Just polling for downstream data
                }
                PacketType::Hello => {
                    debug!("session {} hello", session.id);
                }
                PacketType::Bye => {
                    info!("session {} bye", session.id);
                    sessions.write().await.remove(&session.id);
                }
            }
        }
    }

    // Check for downstream data
    let downstream = {
        let mut queue = session.downstream_queue.lock().await;
        if queue.is_empty() {
            None
        } else {
            // Take all queued data, up to ~3500 bytes (DNS response limit)
            let mut total = 0;
            let mut taken = Vec::new();
            while let Some(data) = queue.first() {
                if total + data.len() > 3500 {
                    break;
                }
                total += data.len();
                taken.push(queue.remove(0));
            }
            if taken.is_empty() {
                None
            } else {
                let mut combined = Vec::with_capacity(total);
                for chunk in taken {
                    combined.extend_from_slice(&chunk);
                }
                Some(combined)
            }
        }
    };

    // Build DNS response
    match downstream {
        Some(data) => {
            let compressed = sp_common::compress::compress(&data);
            let encoded = sp_common::dns_codec::encode_downstream(&compressed);
            let response = build_dns_response(query_wire, encoded.as_bytes())?;
            Ok(response.to_vec())
        }
        None => {
            // Empty TXT response (no data)
            let response = build_dns_response(query_wire, b"")?;
            Ok(response.to_vec())
        }
    }
}

/// Forward data between session and SOCKS5 proxy.
async fn socks_forward_task(
    mut upstream_rx: tokio::sync::mpsc::Receiver<Bytes>,
    session: std::sync::Weak<Session>,
    socks_addr: &str,
) -> Result<()> {
    let socks_stream = TcpStream::connect(socks_addr).await?;
    let (mut socks_read, mut socks_write) = socks_stream.into_split();

    // Forward upstream → SOCKS5
    let write_handle = tokio::spawn(async move {
        while let Some(data) = upstream_rx.recv().await {
            if socks_write.write_all(&data).await.is_err() {
                break;
            }
        }
    });

    // Forward SOCKS5 → downstream queue
    let mut buf = vec![0u8; 4096];
    loop {
        match socks_read.read(&mut buf).await {
            Ok(0) => break, // Connection closed
            Ok(n) => {
                if let Some(session) = session.upgrade() {
                    let packet = TunnelPacket::new_data(0, 0, Bytes::copy_from_slice(&buf[..n]));
                    let encoded = packet.encode();
                    session
                        .downstream_queue
                        .lock()
                        .await
                        .push(encoded);
                } else {
                    break;
                }
            }
            Err(e) => {
                debug!("SOCKS read error: {}", e);
                break;
            }
        }
    }

    write_handle.abort();
    Ok(())
}

/// Build a DNS REFUSED response.
fn build_refused_response(query_wire: &[u8]) -> Result<Vec<u8>> {
    use hickory_proto::op::{Message, MessageType, ResponseCode};
    use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};

    let query = Message::from_bytes(query_wire)?;
    let mut response = Message::new();
    response.set_id(query.id());
    response.set_message_type(MessageType::Response);
    response.set_response_code(ResponseCode::Refused);
    for q in query.queries() {
        response.add_query(q.clone());
    }
    let wire = response.to_bytes()?;
    Ok(wire.to_vec())
}

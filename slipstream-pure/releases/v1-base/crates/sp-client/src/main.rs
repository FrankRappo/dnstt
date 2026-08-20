use anyhow::Result;
use bytes::{Bytes, BytesMut};
use clap::Parser;
use sp_common::compress;
use sp_common::config::{Config, ResolverEndpoint, TransportType};
use sp_common::dns_codec;
use sp_common::protocol::{self, PacketType, TunnelPacket};
use sp_multipath::MultipathRouter;
use sp_transport::dns_tcp::DnsTcpTransport;
use sp_transport::dns_udp::DnsUdpTransport;
use sp_transport::doh::DohTransport;
use sp_transport::doh_ech::DohEchTransport;
use sp_transport::{DnsQuery, Transport, build_dns_query, build_dns_txt_query};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "sp-client", about = "Slipstream Pure DNS Tunnel Client")]
struct Args {
    /// Tunnel domain
    #[arg(long, default_value = "tunnel.example.com")]
    domain: String,

    /// SOCKS5 listen address
    #[arg(long, default_value = "127.0.0.1:7000")]
    listen: String,

    /// Path to resolvers.toml config
    #[arg(long, default_value = "config/resolvers.toml")]
    config: PathBuf,

    /// Server TLS certificate PEM (for verification)
    #[arg(long)]
    cert: Option<PathBuf>,

    /// Use insecure TLS (skip cert verification)
    #[arg(long, default_value_t = true)]
    insecure: bool,

    /// Maximum concurrent connections
    #[arg(long, default_value_t = 64)]
    max_connections: usize,
}

struct TunnelClient {
    domain: String,
    session_id: String,
    router: Arc<MultipathRouter>,
    seq: AtomicU32,
}

impl TunnelClient {
    fn new(domain: String, router: Arc<MultipathRouter>) -> Self {
        Self {
            domain,
            session_id: protocol::generate_session_id(),
            router,
            seq: AtomicU32::new(0),
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Send data upstream (client → server) via DNS query.
    async fn send_upstream(&self, data: &[u8]) -> Result<Option<Vec<u8>>> {
        let packet = TunnelPacket::new_data(0, self.next_seq(), Bytes::copy_from_slice(data));
        let encoded = packet.encode();
        let compressed = compress::compress(&encoded);

        // Encode into DNS name
        let qname = dns_codec::encode_upstream(&compressed, &self.session_id, &self.domain)?;

        // Build and send DNS query
        let query = build_dns_txt_query(&qname)?;
        let (response, _path_id) = self.router.send(&query).await?;

        // Extract downstream data from response
        if let Some(txt_data) = response.txt_data {
            if txt_data.is_empty() {
                return Ok(None);
            }
            let decoded = dns_codec::decode_downstream(
                std::str::from_utf8(&txt_data).unwrap_or(""),
            )?;
            if decoded.is_empty() {
                return Ok(None);
            }
            let decompressed = compress::decompress(&decoded).unwrap_or(decoded);
            Ok(Some(decompressed))
        } else {
            Ok(None)
        }
    }

    /// Poll server for downstream data.
    async fn poll_downstream(&self) -> Result<Option<Vec<u8>>> {
        let packet = TunnelPacket::new_poll(0, self.next_seq());
        let encoded = packet.encode();
        let compressed = compress::compress(&encoded);

        let qname = dns_codec::encode_upstream(&compressed, &self.session_id, &self.domain)?;
        let query = build_dns_txt_query(&qname)?;
        let (response, _) = self.router.send(&query).await?;

        if let Some(txt_data) = response.txt_data {
            if txt_data.is_empty() {
                return Ok(None);
            }
            let decoded = dns_codec::decode_downstream(
                std::str::from_utf8(&txt_data).unwrap_or(""),
            )?;
            if decoded.is_empty() {
                return Ok(None);
            }
            let decompressed = compress::decompress(&decoded).unwrap_or(decoded);
            Ok(Some(decompressed))
        } else {
            Ok(None)
        }
    }

    /// Send session hello.
    async fn send_hello(&self) -> Result<()> {
        let packet = TunnelPacket::new_hello(&self.session_id);
        let encoded = packet.encode();
        let compressed = compress::compress(&encoded);
        let qname = dns_codec::encode_upstream(&compressed, &self.session_id, &self.domain)?;
        let query = build_dns_txt_query(&qname)?;
        self.router.send(&query).await?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sp_client=debug".parse().unwrap()),
        )
        .init();

    let args = Args::parse();
    info!("Slipstream Pure Client starting");
    info!("domain: {}", args.domain);
    info!("SOCKS5 listen: {}", args.listen);
    info!("config: {:?}", args.config);

    // Load resolver config
    let config = Config::load(&args.config)?;
    let endpoints = config.all_endpoints();
    info!("loaded {} resolver endpoints", endpoints.len());

    // Create multipath router and add all paths
    let router = Arc::new(MultipathRouter::new());

    for ep in &endpoints {
        // Create DNS UDP transport for each IP
        match DnsUdpTransport::new(&ep.ip, &ep.name) {
            Ok(transport) => {
                router.add_path(ep.clone(), Arc::new(transport)).await;
            }
            Err(e) => {
                warn!("failed to create DNS-UDP for {} ({}): {}", ep.name, ep.ip, e);
            }
        }

        // Create DNS TCP transport
        match DnsTcpTransport::new(&ep.ip, &ep.name) {
            Ok(transport) => {
                router.add_path(ep.clone(), Arc::new(transport)).await;
            }
            Err(e) => {
                warn!("failed to create DNS-TCP for {} ({}): {}", ep.name, ep.ip, e);
            }
        }

        // Create DoH transport if available
        if let Some(doh_url) = &ep.doh_url {
            match DohTransport::new(doh_url, &ep.name) {
                Ok(transport) => {
                    router.add_path(ep.clone(), Arc::new(transport)).await;
                }
                Err(e) => {
                    warn!("failed to create DoH for {}: {}", ep.name, e);
                }
            }

            // Create DoH+ECH if supported
            if ep.ech_supported {
                match DohEchTransport::new(doh_url, &ep.name) {
                    Ok(transport) => {
                        router.add_path(ep.clone(), Arc::new(transport)).await;
                    }
                    Err(e) => {
                        warn!("failed to create DoH+ECH for {}: {}", ep.name, e);
                    }
                }
            }
        }
    }

    let path_count = router.active_path_count().await;
    info!("initialized {} active paths", path_count);

    if path_count == 0 {
        anyhow::bail!("no active paths — check resolver config");
    }

    // Create tunnel client
    let client = Arc::new(TunnelClient::new(args.domain.clone(), router.clone()));

    // Send hello
    info!("session ID: {}", client.session_id);
    if let Err(e) = client.send_hello().await {
        warn!("hello failed (server may not be running yet): {}", e);
    }

    // Start periodic path reactivation
    let router_reactivate = router.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            router_reactivate.reactivate_all().await;
        }
    });

    // Start SOCKS5 proxy
    let listen_addr: SocketAddr = args.listen.parse()?;
    let listener = TcpListener::bind(listen_addr).await?;
    info!("SOCKS5 proxy listening on {}", listen_addr);

    loop {
        let (stream, peer) = listener.accept().await?;
        let client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_socks5_connection(stream, peer, &client).await {
                debug!("SOCKS5 connection from {} ended: {}", peer, e);
            }
        });
    }
}

/// Handle a SOCKS5 client connection.
async fn handle_socks5_connection(
    mut stream: tokio::net::TcpStream,
    peer: SocketAddr,
    client: &TunnelClient,
) -> Result<()> {
    // SOCKS5 handshake
    let mut buf = [0u8; 258];

    // Read version + nmethods
    stream.read_exact(&mut buf[..2]).await?;
    if buf[0] != 0x05 {
        anyhow::bail!("not SOCKS5: version {}", buf[0]);
    }
    let nmethods = buf[1] as usize;
    stream.read_exact(&mut buf[..nmethods]).await?;

    // Reply: no auth required
    stream.write_all(&[0x05, 0x00]).await?;

    // Read connect request
    stream.read_exact(&mut buf[..4]).await?;
    if buf[0] != 0x05 || buf[1] != 0x01 {
        // Only CONNECT supported
        stream.write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await?;
        anyhow::bail!("unsupported SOCKS5 command: {}", buf[1]);
    }

    let addr_type = buf[3];
    let dest_addr = match addr_type {
        0x01 => {
            // IPv4
            stream.read_exact(&mut buf[..4]).await?;
            format!("{}.{}.{}.{}", buf[0], buf[1], buf[2], buf[3])
        }
        0x03 => {
            // Domain name
            stream.read_exact(&mut buf[..1]).await?;
            let len = buf[0] as usize;
            stream.read_exact(&mut buf[..len]).await?;
            String::from_utf8_lossy(&buf[..len]).to_string()
        }
        0x04 => {
            // IPv6
            stream.read_exact(&mut buf[..16]).await?;
            // Format IPv6
            let mut parts = Vec::new();
            for i in (0..16).step_by(2) {
                parts.push(format!("{:02x}{:02x}", buf[i], buf[i + 1]));
            }
            parts.join(":")
        }
        _ => {
            anyhow::bail!("unsupported address type: {}", addr_type);
        }
    };

    // Read port
    stream.read_exact(&mut buf[..2]).await?;
    let dest_port = u16::from_be_bytes([buf[0], buf[1]]);

    debug!("SOCKS5 CONNECT {}:{} from {}", dest_addr, dest_port, peer);

    // Reply: success (we'll tunnel everything through DNS)
    let reply = [
        0x05, 0x00, 0x00, 0x01, // SOCKS5, success, reserved, IPv4
        0x00, 0x00, 0x00, 0x00, // bind addr 0.0.0.0
        0x00, 0x00, // bind port 0
    ];
    stream.write_all(&reply).await?;

    // Now tunnel data between SOCKS5 client and DNS tunnel
    let (mut read_half, mut write_half) = stream.into_split();

    // Channel for downstream data
    let (downstream_tx, mut downstream_rx) = mpsc::channel::<Vec<u8>>(64);

    // Upstream: read from SOCKS5 client, send via DNS
    let client_ref = client;
    let downstream_tx_upstream = downstream_tx.clone();
    let upstream_handle = tokio::spawn({
        let session_id = client_ref.session_id.clone();
        let domain = client_ref.domain.clone();
        let router = client_ref.router.clone();
        async move {
            let mut buf = vec![0u8; 200]; // Limited by DNS query size
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let data = &buf[..n];
                        // Create tunnel packet
                        let seq = 0u32; // Simplified — real impl uses atomic seq
                        let packet =
                            TunnelPacket::new_data(0, seq, Bytes::copy_from_slice(data));
                        let encoded = packet.encode();
                        let compressed = compress::compress(&encoded);

                        match dns_codec::encode_upstream(&compressed, &session_id, &domain) {
                            Ok(qname) => {
                                if let Ok(query) = build_dns_txt_query(&qname) {
                                    match router.send(&query).await {
                                        Ok((response, _)) => {
                                            // Process downstream from response
                                            if let Some(txt_data) = response.txt_data {
                                                if !txt_data.is_empty() {
                                                    if let Ok(decoded) =
                                                        dns_codec::decode_downstream(
                                                            std::str::from_utf8(&txt_data)
                                                                .unwrap_or(""),
                                                        )
                                                    {
                                                        if !decoded.is_empty() {
                                                            let decompressed =
                                                                compress::decompress(&decoded)
                                                                    .unwrap_or(decoded);
                                                            let _ = downstream_tx_upstream
                                                                .send(decompressed)
                                                                .await;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            debug!("upstream send error: {}", e);
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                debug!("DNS encode error: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        debug!("SOCKS5 read error: {}", e);
                        break;
                    }
                }
            }
        }
    });

    // Downstream polling: periodically poll for server data
    let poll_handle = tokio::spawn({
        let session_id = client_ref.session_id.clone();
        let domain = client_ref.domain.clone();
        let router = client_ref.router.clone();
        let downstream_tx = downstream_tx.clone();
        async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
            loop {
                interval.tick().await;
                let packet = TunnelPacket::new_poll(0, 0);
                let encoded = packet.encode();
                let compressed = compress::compress(&encoded);

                if let Ok(qname) = dns_codec::encode_upstream(&compressed, &session_id, &domain) {
                    if let Ok(query) = build_dns_txt_query(&qname) {
                        if let Ok((response, _)) = router.send(&query).await {
                            if let Some(txt_data) = response.txt_data {
                                if !txt_data.is_empty() {
                                    if let Ok(decoded) = dns_codec::decode_downstream(
                                        std::str::from_utf8(&txt_data).unwrap_or(""),
                                    ) {
                                        if !decoded.is_empty() {
                                            let decompressed =
                                                compress::decompress(&decoded).unwrap_or(decoded);
                                            if downstream_tx.send(decompressed).await.is_err() {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    // Write downstream data to SOCKS5 client
    let write_handle = tokio::spawn(async move {
        while let Some(data) = downstream_rx.recv().await {
            // Decode tunnel packet
            if let Ok(packet) = TunnelPacket::decode(&data) {
                if packet.packet_type == PacketType::Data {
                    if write_half.write_all(&packet.payload).await.is_err() {
                        break;
                    }
                }
            } else {
                // Raw data (not a tunnel packet)
                if write_half.write_all(&data).await.is_err() {
                    break;
                }
            }
        }
    });

    // Wait for upstream to finish (client disconnected)
    let _ = upstream_handle.await;
    poll_handle.abort();
    write_handle.abort();

    Ok(())
}

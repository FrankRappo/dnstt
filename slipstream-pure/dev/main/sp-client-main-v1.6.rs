use anyhow::Result;
use bytes::Bytes;
use clap::Parser;
use sp_common::compress;
use sp_common::config::Config;
use sp_common::dns_codec;
use sp_common::protocol::{self, PacketType, TunnelPacket};
use sp_multipath::MultipathRouter;
use sp_transport::doh::DohTransport;
use sp_transport::doh_ech::DohEchTransport;
use sp_transport::dns_tcp::DnsTcpTransport;
use sp_transport::dns_udp::DnsUdpTransport;
use sp_transport::{build_dns_txt_query, build_dns_txt_query_with_edns0};
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};


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

struct BatchTracker {
    received: std::collections::BTreeSet<u32>,
    ack: u32,
}

impl BatchTracker {
    fn new() -> Self {
        Self { received: std::collections::BTreeSet::new(), ack: 0 }
    }

    fn receive(&mut self, batch_id: u32) -> bool {
        if batch_id == 0 || batch_id <= self.ack { return false; }
        let is_new = self.received.insert(batch_id);
        while self.received.contains(&(self.ack + 1)) {
            self.ack += 1;
            self.received.remove(&self.ack);
        }
        is_new
    }
}

/// Per-stream downstream state with reorder buffer.
/// Fire-and-forget polls may deliver downstream batches out of order;
/// this buffer ensures packets are forwarded to curl in sequence.
struct StreamDownstream {
    tx: mpsc::Sender<Vec<u8>>,
    next_ds_seq: u32,
    reorder_buf: BTreeMap<u32, Vec<u8>>,
    /// When a gap was first detected (for gap timeout)
    gap_since: Option<std::time::Instant>,
}

struct TunnelClient {
    domain: String,
    session_id: String,
    router: Arc<MultipathRouter>,
    seq: AtomicU32,
    stream_id_counter: AtomicU16,
    /// Registered streams: stream_id → downstream state
    streams: Arc<RwLock<HashMap<u16, StreamDownstream>>>,
    /// Maximum safe chunk size for upstream data
    chunk_size: usize,
}

impl TunnelClient {
    fn new(domain: String, router: Arc<MultipathRouter>) -> Self {
        let session_id = protocol::generate_session_id();
        // Upstream payload via QNAME encoding (reliable through all resolvers).
        // EDNS0 options are stripped by most public resolvers, so QNAME is primary.
        // max_upstream_payload = max bytes that fit in DNS QNAME after base32 encoding.
        // Overhead: 9 (header) + ~25 (LZ4 worst case) = 34.
        let max_raw = dns_codec::max_upstream_payload(&session_id, &domain);
        let chunk_size = max_raw.saturating_sub(35).max(30).min(80);
        debug!(
            "max_upstream_payload={}, chunk_size={}",
            max_raw, chunk_size
        );

        Self {
            domain,
            session_id,
            router,
            seq: AtomicU32::new(0),
            stream_id_counter: AtomicU16::new(1),
            streams: Arc::new(RwLock::new(HashMap::new())),
            chunk_size,
        }
    }

    fn next_seq(&self) -> u32 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn next_stream_id(&self) -> u16 {
        self.stream_id_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a stream and return a receiver for downstream data.
    async fn register_stream(&self, stream_id: u16) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel(64);
        self.streams.write().await.insert(stream_id, StreamDownstream {
            tx,
            next_ds_seq: 1,
            reorder_buf: BTreeMap::new(),
            gap_since: None,
        });
        rx
    }

    /// Unregister a stream (called when SOCKS5 connection ends).
    async fn unregister_stream(&self, stream_id: u16) {
        self.streams.write().await.remove(&stream_id);
    }

    /// Send a tunnel packet upstream and dispatch any downstream data from the response.
    /// Retries up to 10 times on transient failures.
    async fn send_packet(&self, packet: &TunnelPacket) -> Result<()> {
        let encoded = packet.encode();
        let compressed = compress::compress(&encoded);

        let query = build_query(&compressed, &self.session_id, &self.domain)?;

        let mut last_err = None;
        for _ in 0..10u8 {
            match self.router.send(&query).await {
                Ok((response, pid)) => {
                    let has_data = response.txt_data.as_ref().map_or(false, |d| !d.is_empty());
                    if has_data {
                        self.dispatch_downstream_raw(response.txt_data.as_ref().unwrap())
                            .await;
                    }
                    return Ok(());
                }
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap())
    }

    /// Send a Connect request for a new stream.
    async fn send_connect(&self, stream_id: u16, host: &str, port: u16) -> Result<()> {
        let packet = TunnelPacket::new_connect(stream_id, host, port);
        self.send_packet(&packet).await
    }

    /// Send data on a stream.
    #[allow(dead_code)]
    async fn send_data(&self, stream_id: u16, data: &[u8]) -> Result<()> {
        let packet =
            TunnelPacket::new_data(stream_id, self.next_seq(), Bytes::copy_from_slice(data));
        self.send_packet(&packet).await
    }

    /// Send a Disconnect for a stream.
    async fn send_disconnect(&self, stream_id: u16) -> Result<()> {
        let packet = TunnelPacket::new_disconnect(stream_id);
        self.send_packet(&packet).await
    }

    /// Poll server for downstream data and dispatch it.
    async fn poll_and_dispatch(&self) -> Result<()> {
        let packet = TunnelPacket::new_poll(0, self.next_seq());
        self.send_packet(&packet).await
    }

    /// Send session hello.
    async fn send_hello(&self) -> Result<()> {
        let packet = TunnelPacket::new_hello(&self.session_id);
        self.send_packet(&packet).await
    }

    /// Decode + decompress raw TXT data and dispatch individual packets to streams.
    /// Handles batch_id prefix (4 bytes) from streaming delivery.
    async fn dispatch_downstream_raw(&self, txt_data: &[u8]) {
        debug!("dispatch_downstream_raw: {} bytes TXT data", txt_data.len());
        let txt_str = match std::str::from_utf8(txt_data) {
            Ok(s) => s,
            Err(e) => {
                debug!("TXT data not valid UTF-8: {}", e);
                return;
            }
        };
        let decoded = match dns_codec::decode_downstream(txt_str) {
            Ok(d) => d,
            Err(e) => {
                debug!("base64 decode error: {}", e);
                return;
            }
        };
        if decoded.is_empty() {
            return;
        }
        let decompressed = compress::decompress(&decoded).unwrap_or(decoded);
        debug!(
            "dispatch_downstream: {} bytes decompressed",
            decompressed.len()
        );
        // Strip 4-byte batch_id prefix if present (added by server sliding window)
        let packet_data = if decompressed.len() > 4 {
            &decompressed[4..]
        } else {
            &decompressed
        };
        self.dispatch_downstream(packet_data).await;
    }

    /// Parse concatenated TunnelPackets and route each to the appropriate stream.
    /// Uses a per-stream reorder buffer to handle out-of-order delivery.
    async fn dispatch_downstream(&self, data: &[u8]) {
        let packets = TunnelPacket::decode_all(data);
        let mut streams = self.streams.write().await;
        for packet in packets {
            match packet.packet_type {
                PacketType::Data => {
                    if let Some(state) = streams.get_mut(&packet.stream_id) {
                        dispatch_ds_packet(state, packet.seq_num, packet.payload.to_vec()).await;
                    }
                }
                PacketType::Disconnect => {
                    if let Some(state) = streams.get(&packet.stream_id) {
                        let _ = state.tx.send(Vec::new()).await;
                    }
                }
                _ => {}
            }
        }
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
        match DnsUdpTransport::new(&ep.ip, &ep.name) {
            Ok(transport) => {
                router.add_path(ep.clone(), Arc::new(transport)).await;
            }
            Err(e) => {
                warn!(
                    "failed to create DNS-UDP for {} ({}): {}",
                    ep.name, ep.ip, e
                );
            }
        }

        match DnsTcpTransport::new(&ep.ip, &ep.name) {
            Ok(transport) => {
                router.add_path(ep.clone(), Arc::new(transport)).await;
            }
            Err(e) => {
                warn!(
                    "failed to create DNS-TCP for {} ({}): {}",
                    ep.name, ep.ip, e
                );
            }
        }

        if let Some(doh_url) = &ep.doh_url {
            match DohTransport::new(doh_url, &ep.name) {
                Ok(transport) => {
                    router.add_path(ep.clone(), Arc::new(transport)).await;
                }
                Err(e) => {
                    warn!("failed to create DoH for {}: {}", ep.name, e);
                }
            }

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

    // Discover good paths by sending Hello through ALL paths in parallel.
    // Server echoes back data; paths that deliver the echo are marked as good.
    info!("session ID: {}", client.session_id);
    info!("chunk_size: {}", client.chunk_size);
    info!("discovering good paths...");
    {
        let packet = TunnelPacket::new_hello(&client.session_id);
        let encoded = packet.encode();
        let compressed = compress::compress(&encoded);
        if let Ok(query) = build_query(&compressed, &client.session_id, &client.domain) {
            let count = router.discover_good_paths(&query).await;
            info!("discovered {} good paths", count);
        }
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

    // Downstream polling is now handled inline by each stream's upstream task,
    // using known-good resolver paths. No separate poll task needed.

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
        stream
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
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

    // Reply: success (so SOCKS5 client starts sending data, e.g. TLS ClientHello)
    let reply = [
        0x05, 0x00, 0x00, 0x01, // SOCKS5, success, reserved, IPv4
        0x00, 0x00, 0x00, 0x00, // bind addr 0.0.0.0
        0x00, 0x00, // bind port 0
    ];
    stream.write_all(&reply).await?;

    // Read initial client data (TLS ClientHello ~200-500 bytes) before splitting.
    // This is buffered so we can replay it on retry if the first attempt times out.
    let mut initial_buf = vec![0u8; 4096];
    let initial_data = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        stream.read(&mut initial_buf),
    ).await {
        Ok(Ok(n)) if n > 0 => initial_buf[..n].to_vec(),
        _ => Vec::new(),
    };
    debug!("read {} bytes initial client data", initial_data.len());

    let (mut read_half, mut write_half) = stream.into_split();
    let chunk_size = client.chunk_size;

    // === CONNECTION-LEVEL RETRY ===
    // HTTPS TLS ServerHello (~3252 bytes = ~14 DNS responses) often fails due to
    // DNS packet loss. Retry the entire Connect + initial data + poll cycle up to 3 times.
    let max_attempts = 5u8;
    let mut final_stream_id = 0u16;
    let mut final_downstream_rx: Option<mpsc::Receiver<Vec<u8>>> = None;
    let mut final_pump_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut _final_batch_id = Arc::new(AtomicU32::new(0));
    let mut _final_batch_tracker: Option<Arc<tokio::sync::Mutex<BatchTracker>>> = None;

    for attempt in 1..=max_attempts {
        let stream_id = client.next_stream_id();
        debug!("attempt {}/{}: stream_id {} for {}:{}", attempt, max_attempts, stream_id, dest_addr, dest_port);

        let mut downstream_rx = client.register_stream(stream_id).await;

        // Send Connect request (with sub-retry up to 6)
        let mut connect_ok = false;
        for c_attempt in 1..=6u8 {
            match client.send_connect(stream_id, &dest_addr, dest_port).await {
                Ok(_) => { connect_ok = true; break; }
                Err(e) => {
                    debug!("Connect sub-attempt {}/6 failed: {}", c_attempt, e);
                    if c_attempt < 6 {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            }
        }
        if !connect_ok {
            client.unregister_stream(stream_id).await;
            if attempt == max_attempts {
                anyhow::bail!("failed to send Connect after {} retry attempts", max_attempts);
            }
            continue;
        }

        // Start poll pump BEFORE sending data (so it's ready to catch the response)
        let batch_id = Arc::new(AtomicU32::new(0));
        let batch_tracker = Arc::new(tokio::sync::Mutex::new(BatchTracker::new()));
        let pump_handle = tokio::spawn({
            let session_id = client.session_id.clone();
            let domain = client.domain.clone();
            let router = client.router.clone();
            let streams = client.streams.clone();
            let batch_id = batch_id.clone();
            let batch_tracker = batch_tracker.clone();
            async move {
                let mut poll_seq: u32 = 1_000_000;
                loop {
                    let ack = batch_id.load(Ordering::Relaxed);
                    let ack_payload = Bytes::copy_from_slice(&ack.to_be_bytes());
                    let pkt = TunnelPacket {
                        packet_type: PacketType::Poll,
                        stream_id: 0,
                        seq_num: poll_seq,
                        payload: ack_payload,
                    };
                    poll_seq = poll_seq.wrapping_add(1);
                    let encoded = pkt.encode();
                    let compressed = compress::compress(&encoded);

                    let query = match build_query(&compressed, &session_id, &domain) {
                        Ok(q) => q,
                        Err(_) => continue,
                    };

                    let result = match tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        router.send_parallel_good(&query, 15),
                    ).await {
                        Ok(r) => r,
                        Err(_) => continue,
                    };

                    match result {
                        Ok((resp, pid)) => {
                            let has_data = resp.txt_data.as_ref().map_or(false, |d| !d.is_empty());
                            if has_data {
                                let recv_id = dispatch_downstream_with_batch(
                                    resp.txt_data.as_ref().unwrap(),
                                    &streams,
                                    &batch_tracker,
                                ).await;
                                if recv_id > 0 {
                                    router.report_downstream_success(pid).await;
                                    let t = batch_tracker.lock().await;
                                    let new_ack = t.ack;
                                    batch_id.store(new_ack, Ordering::Relaxed);
                                    debug!("pump: recv batch={} ack_now={} pending={:?}",
                                        recv_id, new_ack, t.received.iter().collect::<Vec<_>>());
                                } else {
                                    let ack = batch_id.load(Ordering::Relaxed);
                                    debug!("pump: dup/old response, current_ack={}", ack);
                                }
                            } else {
                                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                            }
                        }
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    }
                }
            }
        });

        // Send buffered initial client data (ClientHello) through multiple paths in parallel
        if !initial_data.is_empty() {
            let chunk_count = initial_data.chunks(chunk_size).count();
            let seq_base: u32 = 1;
            for (i, chunk) in initial_data.chunks(chunk_size).enumerate() {
                let s = seq_base.wrapping_add(i as u32);
                let chunk_data = Bytes::copy_from_slice(chunk);
                let router_c = client.router.clone();
                let sid = client.session_id.clone();
                let dom = client.domain.clone();
                tokio::spawn(async move {
                    let packet = TunnelPacket::new_data(stream_id, s, chunk_data);
                    let encoded = packet.encode();
                    let compressed = compress::compress(&encoded);
                    let query = match build_query(&compressed, &sid, &dom) {
                        Ok(q) => q,
                        Err(_) => return,
                    };
                    let _ = router_c.send_parallel_good(&query, 10).await;
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            debug!("spawned {} initial data chunks in parallel", chunk_count);
        }

        // Wait for first downstream data (3s timeout)
        let first = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            downstream_rx.recv(),
        ).await;

        match first {
            Ok(Some(data)) if !data.is_empty() => {
                if write_half.write_all(&data).await.is_err() {
                    pump_handle.abort();
                    client.unregister_stream(stream_id).await;
                    let _ = client.send_disconnect(stream_id).await;
                    anyhow::bail!("failed to write first downstream to SOCKS5 client");
                }
                info!("attempt {}: got {} bytes downstream for {}:{}", attempt, data.len(), dest_addr, dest_port);
                final_stream_id = stream_id;
                final_downstream_rx = Some(downstream_rx);
                final_pump_handle = Some(pump_handle);
                _final_batch_id = batch_id;
                _final_batch_tracker = Some(batch_tracker);
                break;
            }
            _ => {
                pump_handle.abort();
                client.unregister_stream(stream_id).await;
                let _ = client.send_disconnect(stream_id).await;
                if attempt < max_attempts {
                    info!("retry {}/{}: no downstream within 5s for {}:{}", attempt, max_attempts, dest_addr, dest_port);
                }
            }
        }
    }

    let mut downstream_rx = match final_downstream_rx {
        Some(rx) => rx,
        None => anyhow::bail!("no downstream after {} attempts for {}:{}", max_attempts, dest_addr, dest_port),
    };
    let pump_handle = final_pump_handle.unwrap();
    let stream_id = final_stream_id;

    // === UPSTREAM: read from SOCKS5 client, send via DNS (fire-and-forget) ===
    // Seq starts after initial_data chunks (already sent during retry phase)
    let initial_chunks_sent = if initial_data.is_empty() { 0u32 } else {
        ((initial_data.len() + chunk_size - 1) / chunk_size) as u32
    };
    let upstream_handle = tokio::spawn({
        let session_id = client.session_id.clone();
        let domain = client.domain.clone();
        let router = client.router.clone();
        async move {
            let mut buf = vec![0u8; chunk_size * 8];
            let mut seq: u32 = 1 + initial_chunks_sent;

            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        let num_chunks = (n + chunk_size - 1) / chunk_size;
                        let base_seq = seq;
                        seq = seq.wrapping_add(num_chunks as u32);
                        if seq == 0 { seq = 1; }

                        for (i, chunk) in data.chunks(chunk_size).enumerate() {
                            let s = base_seq.wrapping_add(i as u32);
                            let chunk_data = Bytes::copy_from_slice(chunk);
                            let router_c = router.clone();
                            let sid = session_id.clone();
                            let dom = domain.clone();

                            tokio::spawn(async move {
                                let packet = TunnelPacket::new_data(stream_id, s, chunk_data);
                                let encoded = packet.encode();
                                let compressed = compress::compress(&encoded);
                                let query = match build_query(&compressed, &sid, &dom) {
                                    Ok(q) => q,
                                    Err(e) => { debug!("encode error: {}", e); return; }
                                };
                                for _attempt in 0..5u8 {
                                    match router_c.send_via_good_paths(&query).await {
                                        Ok(_) => break,
                                        Err(e) => debug!("send error: {}", e),
                                    }
                                }
                            });
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

    // === DOWNSTREAM: write data to SOCKS5 client with idle timeout ===
    let write_handle = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(8),
                downstream_rx.recv(),
            ).await {
                Ok(Some(data)) if !data.is_empty() => {
                    if write_half.write_all(&data).await.is_err() {
                        break;
                    }
                }
                _ => break, // timeout, closed, or empty = done
            }
        }
    });

    // Wait for either upstream or downstream to finish
    tokio::select! {
        _ = upstream_handle => {},
        _ = write_handle => {},
    }
    pump_handle.abort();

    // Clean up
    client.unregister_stream(stream_id).await;
    let _ = client.send_disconnect(stream_id).await;

    Ok(())
}

/// Forward a downstream Data packet through a stream's reorder buffer.
/// Packets are forwarded in sequence; out-of-order packets are buffered.
/// Gap timeout: if a gap persists > 3 seconds, skip to next available seq.
async fn dispatch_ds_packet(state: &mut StreamDownstream, seq: u32, data: Vec<u8>) {
    // Duplicate check
    if seq > 0 && seq < state.next_ds_seq {
        return;
    }
    if seq == state.next_ds_seq {
        // In order — forward immediately, then flush consecutive buffered packets
        let _ = state.tx.send(data).await;
        state.next_ds_seq += 1;
        state.gap_since = None;
        while let Some(buffered) = state.reorder_buf.remove(&state.next_ds_seq) {
            let _ = state.tx.send(buffered).await;
            state.next_ds_seq += 1;
        }
    } else if seq > state.next_ds_seq {
        // Out of order — buffer
        debug!(
            "ds reorder: buffering seq {} (expect {})",
            seq, state.next_ds_seq
        );
        state.reorder_buf.insert(seq, data);

        // Gap timeout: skip missing seqs after 4 seconds (must be > first downstream timeout)
        if let Some(since) = state.gap_since {
            if since.elapsed() > std::time::Duration::from_secs(4) {
                if let Some((&next_avail, _)) = state.reorder_buf.iter().next() {
                    warn!("ds gap timeout: skipping seq {} to {}", state.next_ds_seq, next_avail);
                    state.next_ds_seq = next_avail;
                    state.gap_since = None;
                    while let Some(buffered) = state.reorder_buf.remove(&state.next_ds_seq) {
                        let _ = state.tx.send(buffered).await;
                        state.next_ds_seq += 1;
                    }
                }
            }
        } else {
            state.gap_since = Some(std::time::Instant::now());
        }
    }
}

/// Build a DNS query from compressed tunnel data.
/// Uses QNAME encoding (reliable through all public resolvers).
/// EDNS0 upstream is available but stripped by most resolvers.
fn build_query(compressed: &[u8], session_id: &str, domain: &str) -> Result<sp_transport::DnsQuery> {
    let qname = dns_codec::encode_upstream(compressed, session_id, domain)?;
    build_dns_txt_query(&qname)
}

async fn dispatch_downstream_with_batch(
    txt_data: &[u8],
    streams: &Arc<RwLock<HashMap<u16, StreamDownstream>>>,
    tracker: &tokio::sync::Mutex<BatchTracker>,
) -> u32 {
    let txt_str = match std::str::from_utf8(txt_data) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let decoded = match dns_codec::decode_downstream(txt_str) {
        Ok(d) => d,
        Err(_) => return 0,
    };
    if decoded.is_empty() { return 0; }
    let decompressed = compress::decompress(&decoded).unwrap_or(decoded);
    if decompressed.len() < 4 { return 0; }

    let server_batch_id = u32::from_be_bytes([
        decompressed[0], decompressed[1], decompressed[2], decompressed[3],
    ]);
    let packet_data = &decompressed[4..];

    {
        let mut t = tracker.lock().await;
        if !t.receive(server_batch_id) {
            return 0;
        }
    }

    let packets = TunnelPacket::decode_all(packet_data);
    let mut streams = streams.write().await;
    for packet in packets {
        match packet.packet_type {
            PacketType::Data => {
                if let Some(state) = streams.get_mut(&packet.stream_id) {
                    dispatch_ds_packet(state, packet.seq_num, packet.payload.to_vec()).await;
                }
            }
            PacketType::Disconnect => {
                if let Some(state) = streams.get(&packet.stream_id) {
                    let _ = state.tx.send(Vec::new()).await;
                }
            }
            _ => {}
        }
    }
    server_batch_id
}

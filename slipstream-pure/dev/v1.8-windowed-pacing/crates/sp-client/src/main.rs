mod background_scanner;
mod resolver_cache;

use anyhow::Result;
use bytes::Bytes;
use clap::Parser;
use sp_common::compress;
use sp_common::config::{self, Config, ResolverConfig};
use sp_common::dns_codec;
use sp_common::protocol::{self, PacketType, TunnelPacket};
use sp_multipath::{MultipathRouter, PathId};
use sp_transport::doh::DohTransport;
use sp_transport::doh_ech::DohEchTransport;
use sp_transport::dns_tcp::DnsTcpTransport;
use sp_transport::dns_udp::DnsUdpTransport;
use sp_transport::{build_dns_txt_query, DnsResponse};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, warn};

#[derive(Parser, Debug)]
#[command(name = "sp-client", about = "Slipstream Pure DNS Tunnel Client v3.0")]
struct Args {
    #[arg(long, default_value = "tunnel.example.com")]
    domain: String,
    #[arg(long, default_value = "127.0.0.1:7000")]
    listen: String,
    #[arg(long, default_value = "config/resolvers.toml")]
    config: PathBuf,
    #[arg(long)]
    cert: Option<PathBuf>,
    #[arg(long, default_value_t = true)]
    insecure: bool,
    #[arg(long, default_value_t = 64)]
    max_connections: usize,
    #[arg(long, default_value_t = false)]
    auto_discover: bool,
    #[arg(long, default_value = ".")]
    cache_dir: PathBuf,
    #[arg(long)]
    hint: Vec<String>,
    #[arg(long, default_value_t = false)]
    discovery_only: bool,
}

struct BatchTracker {
    received: BTreeSet<u32>,
    ack: u32,
}

impl BatchTracker {
    fn new() -> Self { Self { received: BTreeSet::new(), ack: 0 } }
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

struct StreamDownstream {
    tx: mpsc::Sender<Vec<u8>>,
    next_ds_seq: u32,
    reorder_buf: BTreeMap<u32, Vec<u8>>,
    gap_since: Option<std::time::Instant>,
}

struct TunnelClient {
    domain: String,
    session_id: String,
    router: Arc<MultipathRouter>,
    seq: AtomicU32,
    stream_id_counter: AtomicU16,
    streams: Arc<RwLock<HashMap<u16, StreamDownstream>>>,
    chunk_size: usize,
    ds_max_raw: usize,
    ds_chunk_size: usize,
    /// Dynamic: INFLIGHT = good_paths, WINDOW = INFLIGHT*2+2
    inflight: usize,
    window_size: usize,
}

impl TunnelClient {
    fn new(domain: String, router: Arc<MultipathRouter>, endpoints: &[config::ResolverEndpoint], good_paths: usize) -> Self {
        let session_id = protocol::generate_session_id();
        let max_raw = dns_codec::max_upstream_payload(&session_id, &domain);
        let chunk_size = max_raw.saturating_sub(35).max(30).min(80);

        let ds_max_raw = endpoints.iter().map(|ep| ep.optimal_max_raw()).min().unwrap_or(1000);
        let ds_chunk_size = endpoints.iter().map(|ep| ep.optimal_chunk_size()).min().unwrap_or(480);

        // INFLIGHT = good_paths (1 poll per path). WINDOW = INFLIGHT + 1.
        // Research proved: WINDOW = PIPELINE + 1 is optimal (extra slot = retransmit buffer).
        let inflight = good_paths.max(2).min(200);
        let window_size = (inflight + 1).max(4);

        info!("session {}: inflight={}, window={}, max_raw={}, chunk={}",
            session_id, inflight, window_size, ds_max_raw, ds_chunk_size);

        Self {
            domain, session_id, router,
            seq: AtomicU32::new(0),
            stream_id_counter: AtomicU16::new(1),
            streams: Arc::new(RwLock::new(HashMap::new())),
            chunk_size, ds_max_raw, ds_chunk_size,
            inflight, window_size,
        }
    }

    fn next_stream_id(&self) -> u16 { self.stream_id_counter.fetch_add(1, Ordering::Relaxed) }

    async fn register_stream(&self, stream_id: u16) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel(64);
        self.streams.write().await.insert(stream_id, StreamDownstream {
            tx, next_ds_seq: 1, reorder_buf: BTreeMap::new(), gap_since: None,
        });
        rx
    }

    async fn unregister_stream(&self, stream_id: u16) {
        self.streams.write().await.remove(&stream_id);
    }

    async fn send_packet(&self, packet: &TunnelPacket) -> Result<()> {
        let encoded = packet.encode();
        let compressed = compress::compress(&encoded);
        let query = build_query(&compressed, &self.session_id, &self.domain)?;
        for _ in 0..10u8 {
            match self.router.send(&query).await {
                Ok((response, _)) => {
                    if let Some(txt) = response.txt_data.as_ref() {
                        if !txt.is_empty() { self.dispatch_downstream_raw(txt).await; }
                    }
                    return Ok(());
                }
                Err(_) => {}
            }
        }
        anyhow::bail!("send failed after 10 attempts")
    }

    async fn send_connect(&self, stream_id: u16, host: &str, port: u16) -> Result<()> {
        self.send_packet(&TunnelPacket::new_connect(stream_id, host, port)).await
    }

    async fn send_disconnect(&self, stream_id: u16) -> Result<()> {
        self.send_packet(&TunnelPacket::new_disconnect(stream_id)).await
    }

    async fn dispatch_downstream_raw(&self, txt_data: &[u8]) {
        let txt_str = match std::str::from_utf8(txt_data) { Ok(s) => s, Err(_) => return };
        let decoded = match dns_codec::decode_downstream(txt_str) { Ok(d) => d, Err(_) => return };
        if decoded.is_empty() { return; }
        let decompressed = compress::decompress(&decoded).unwrap_or(decoded);
        let packet_data = if decompressed.len() > 4 { &decompressed[4..] } else { &decompressed };
        self.dispatch_downstream(packet_data).await;
    }

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

async fn add_paths_to_router(router: &MultipathRouter, endpoints: &[config::ResolverEndpoint]) {
    for ep in endpoints {
        if let Ok(t) = DnsUdpTransport::new(&ep.ip, &ep.name) {
            router.add_path(ep.clone(), Arc::new(t)).await;
        }
        if let Ok(t) = DnsTcpTransport::new(&ep.ip, &ep.name) {
            router.add_path(ep.clone(), Arc::new(t)).await;
        }
        if let Some(doh_url) = &ep.doh_url {
            if let Ok(t) = DohTransport::new(doh_url, &ep.name) {
                router.add_path(ep.clone(), Arc::new(t)).await;
            }
            if ep.ech_supported {
                if let Ok(t) = DohEchTransport::new(doh_url, &ep.name) {
                    router.add_path(ep.clone(), Arc::new(t)).await;
                }
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
    info!("Slipstream Pure Client v3.0 (single-session scaling) starting");
    info!("domain: {}, listen: {}", args.domain, args.listen);

    // === Collect resolvers ===
    let mut all_resolvers: Vec<ResolverConfig> = Vec::new();
    if !args.discovery_only {
        match Config::load(&args.config) {
            Ok(config) => {
                info!("loaded {} resolvers from config", config.resolvers.len());
                all_resolvers.extend(config.resolvers);
            }
            Err(e) => {
                if args.auto_discover { warn!("no config: {}", e); }
                else { return Err(e); }
            }
        }
    }
    if args.auto_discover {
        if let Some(cached) = resolver_cache::load_cache(&args.cache_dir) {
            info!("loaded {} cached resolvers", cached.resolvers.len());
            all_resolvers.extend(cached.resolvers);
        }
    }
    // Bootstrap with hints (if provided)
    for hint in &args.hint {
        if let Some(ip) = hint.strip_prefix("dns:") {
            let deleg = sp_scanner::prober::test_delegation(ip, None, &args.domain).await;
            if deleg.works {
                info!("hint dns:{} — delegation works!", ip);
                all_resolvers.push(ResolverConfig {
                    name: format!("hint-{}", ip), ip: vec![ip.to_string()],
                    doh: None, ech: false, edns0_max: None, rate_limit_rps: None,
                });
            } else { warn!("hint dns:{} — delegation failed", ip); }
        } else if let Some(doh_url) = hint.strip_prefix("doh:") {
            let deleg = sp_scanner::prober::test_delegation("", Some(doh_url), &args.domain).await;
            if deleg.works {
                info!("hint doh:{} — delegation works!", doh_url);
                all_resolvers.push(ResolverConfig {
                    name: "hint-doh".to_string(), ip: vec![],
                    doh: Some(doh_url.to_string()), ech: false, edns0_max: None, rate_limit_rps: None,
                });
            } else { warn!("hint doh:{} — delegation failed", doh_url); }
        } else { warn!("unknown hint format: {} (expected dns:IP or doh:URL)", hint); }
    }

    // Auto-discover from bundled list if still empty
    if all_resolvers.is_empty() && args.auto_discover {
        warn!("no resolvers from config or cache, scanning bundled list...");
        let bundled = sp_scanner::resolvers::bundled_resolvers();
        let sem = Arc::new(tokio::sync::Semaphore::new(20));
        let domain = args.domain.clone();
        let mut tasks = Vec::new();
        for r in &bundled {
            let sem = sem.clone();
            let domain = domain.clone();
            let ip = r.ip.to_string();
            let name = r.name.to_string();
            let doh = r.doh_url.map(|s| s.to_string());
            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let deleg = sp_scanner::prober::test_delegation(&ip, doh.as_deref(), &domain).await;
                if deleg.works {
                    Some(ResolverConfig {
                        name, ip: vec![ip], doh, ech: false,
                        edns0_max: None,
                        rate_limit_rps: None,
                    })
                } else { None }
            }));
        }
        for task in tasks {
            if let Ok(Some(r)) = task.await {
                all_resolvers.push(r);
            }
        }
        info!("auto-discovered {} working resolvers from {} bundled", all_resolvers.len(), bundled.len());
    }

    all_resolvers.sort_by(|a, b| a.ip.cmp(&b.ip));
    all_resolvers.dedup_by(|a, b| a.ip == b.ip);

    if all_resolvers.is_empty() {
        anyhow::bail!("no resolvers found");
    }
    info!("using {} resolvers", all_resolvers.len());

    let config = Config { resolvers: all_resolvers };
    let endpoints = config.all_endpoints();

    // === Single router with ALL resolvers ===
    let router = Arc::new(MultipathRouter::new());
    add_paths_to_router(&router, &endpoints).await;

    // === Discovery: parallel probe ===
    info!("discovering good paths...");
    let probe_sid = protocol::generate_session_id();
    let probe = TunnelPacket::new_hello(&probe_sid);
    let encoded = probe.encode();
    let compressed = compress::compress(&encoded);
    let good_paths = if let Ok(query) = build_query(&compressed, &probe_sid, &args.domain) {
        router.discover_good_paths(&query).await
    } else { 0 };
    info!("discovered {} good paths", good_paths);

    if good_paths == 0 {
        anyhow::bail!("no good paths found");
    }

    // === Create client with dynamic INFLIGHT/WINDOW ===
    let client = Arc::new(TunnelClient::new(
        args.domain.clone(), router.clone(), &endpoints, good_paths,
    ));

    // === Send Hello with window size ===
    {
        let hello_payload = format!("{} max_raw={} chunk={} window={}",
            client.session_id, client.ds_max_raw, client.ds_chunk_size, client.window_size);
        let packet = TunnelPacket {
            packet_type: PacketType::Hello,
            stream_id: 0, seq_num: 0,
            payload: Bytes::from(hello_payload),
        };
        let encoded = packet.encode();
        let compressed = compress::compress(&encoded);
        if let Ok(query) = build_query(&compressed, &client.session_id, &client.domain) {
            let _ = router.discover_good_paths(&query).await;
        }
    }

    info!("SOCKS5 proxy ready: inflight={}, window={}", client.inflight, client.window_size);

    // Periodic path reactivation
    let router_r = router.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop { interval.tick().await; router_r.reactivate_all().await; }
    });

    // Background scanner + hot-swap + network change detection
    if args.auto_discover {
        let (bg_scanner, mut new_resolver_rx) = background_scanner::BackgroundScanner::new(
            args.domain.clone(),
            router.clone(),
            args.cache_dir.clone(),
        );
        tokio::spawn(bg_scanner.run());

        // Hot-swap: add new resolvers to router as scanner finds them
        let router_hotswap = router.clone();
        tokio::spawn(async move {
            while new_resolver_rx.changed().await.is_ok() {
                let new_resolvers = new_resolver_rx.borrow().clone();
                for r in &new_resolvers {
                    for ip in &r.ip {
                        let ep = sp_common::config::ResolverEndpoint {
                            name: r.name.clone(), ip: ip.clone(),
                            doh_url: r.doh.clone(), ech_supported: r.ech,
                            edns0_max: r.edns0_max, rate_limit_rps: r.rate_limit_rps,
                        };
                        if let Ok(t) = DnsUdpTransport::new(ip, &r.name) {
                            router_hotswap.add_path(ep.clone(), Arc::new(t)).await;
                        }
                        if let Some(ref doh_url) = r.doh {
                            if let Ok(t) = DohTransport::new(doh_url, &r.name) {
                                router_hotswap.add_path(ep.clone(), Arc::new(t)).await;
                            }
                        }
                    }
                }
                info!("[hotswap] added {} new resolvers", new_resolvers.len());
            }
        });

        // Network change detection
        let router_netchange = router.clone();
        let domain_netchange = args.domain.clone();
        tokio::spawn(async move {
            let mut current_dns = sp_scanner::detect::read_system_dns();
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let (changed, new_dns) = background_scanner::detect_network_change(&current_dns);
                if changed {
                    warn!("[network] change detected: {:?} → {:?}", current_dns, new_dns);
                    current_dns = new_dns;
                    let pkt = TunnelPacket::new_hello(&"re-bootstrap");
                    let enc = pkt.encode();
                    let comp = compress::compress(&enc);
                    if let Ok(q) = build_query(&comp, &"re-bootstrap", &domain_netchange) {
                        let c = router_netchange.discover_good_paths(&q).await;
                        info!("[network] re-discovered {} paths", c);
                    }
                }
            }
        });
    }

    let listen_addr: SocketAddr = args.listen.parse()?;
    let listener = TcpListener::bind(listen_addr).await?;
    info!("SOCKS5 listening on {}", listen_addr);

    loop {
        let (stream, peer) = listener.accept().await?;
        let client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_socks5(stream, peer, &client).await {
                debug!("SOCKS5 {} ended: {}", peer, e);
            }
        });
    }
}

async fn handle_socks5(
    mut stream: tokio::net::TcpStream,
    peer: SocketAddr,
    client: &TunnelClient,
) -> Result<()> {
    // SOCKS5 handshake
    let mut buf = [0u8; 258];
    stream.read_exact(&mut buf[..2]).await?;
    if buf[0] != 0x05 { anyhow::bail!("not SOCKS5"); }
    let nm = buf[1] as usize;
    stream.read_exact(&mut buf[..nm]).await?;
    stream.write_all(&[0x05, 0x00]).await?;

    stream.read_exact(&mut buf[..4]).await?;
    if buf[0] != 0x05 || buf[1] != 0x01 {
        stream.write_all(&[0x05, 0x07, 0x00, 0x01, 0,0,0,0, 0,0]).await?;
        anyhow::bail!("unsupported cmd");
    }

    let dest_addr = match buf[3] {
        0x01 => { stream.read_exact(&mut buf[..4]).await?; format!("{}.{}.{}.{}", buf[0],buf[1],buf[2],buf[3]) }
        0x03 => { stream.read_exact(&mut buf[..1]).await?; let l=buf[0] as usize; stream.read_exact(&mut buf[..l]).await?; String::from_utf8_lossy(&buf[..l]).to_string() }
        0x04 => { stream.read_exact(&mut buf[..16]).await?; let mut p=Vec::new(); for i in (0..16).step_by(2) { p.push(format!("{:02x}{:02x}",buf[i],buf[i+1])); } p.join(":") }
        _ => anyhow::bail!("bad addr type"),
    };
    stream.read_exact(&mut buf[..2]).await?;
    let dest_port = u16::from_be_bytes([buf[0], buf[1]]);
    debug!("CONNECT {}:{} from {}", dest_addr, dest_port, peer);
    stream.write_all(&[0x05,0x00,0x00,0x01, 0,0,0,0, 0,0]).await?;

    // Buffer initial client data
    let mut ibuf = vec![0u8; 4096];
    let initial_data = match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut ibuf)).await {
        Ok(Ok(n)) if n > 0 => ibuf[..n].to_vec(),
        _ => Vec::new(),
    };

    let (mut read_half, mut write_half) = stream.into_split();
    let chunk_size = client.chunk_size;

    // Connection-level retry
    let max_attempts = 5u8;
    let mut final_stream_id = 0u16;
    let mut final_rx: Option<mpsc::Receiver<Vec<u8>>> = None;
    let mut final_pump: Option<tokio::task::JoinHandle<()>> = None;

    for attempt in 1..=max_attempts {
        let stream_id = client.next_stream_id();
        let mut downstream_rx = client.register_stream(stream_id).await;

        let mut connect_ok = false;
        for c in 1..=6u8 {
            match client.send_connect(stream_id, &dest_addr, dest_port).await {
                Ok(_) => { connect_ok = true; break; }
                Err(e) => { debug!("Connect {}/6: {}", c, e); if c < 6 { tokio::time::sleep(Duration::from_millis(500)).await; } }
            }
        }
        if !connect_ok { client.unregister_stream(stream_id).await; continue; }

        // Start streaming pump
        let pump = tokio::spawn(pump_task(
            client.session_id.clone(), client.domain.clone(),
            client.router.clone(), client.streams.clone(),
            Arc::new(AtomicU32::new(0)), Arc::new(Mutex::new(BatchTracker::new())),
            client.inflight, client.router.clone(),
        ));

        // Send initial data
        if !initial_data.is_empty() {
            for (i, chunk) in initial_data.chunks(chunk_size).enumerate() {
                let s = 1u32.wrapping_add(i as u32);
                let cd = Bytes::copy_from_slice(chunk);
                let r = client.router.clone();
                let sid = client.session_id.clone();
                let dom = client.domain.clone();
                tokio::spawn(async move {
                    let pkt = TunnelPacket::new_data(stream_id, s, cd);
                    let enc = pkt.encode();
                    let comp = compress::compress(&enc);
                    if let Ok(q) = build_query(&comp, &sid, &dom) {
                        let _ = r.send_parallel_good(&q, 3).await;
                    }
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Wait for first downstream
        match tokio::time::timeout(Duration::from_secs(5), downstream_rx.recv()).await {
            Ok(Some(data)) if !data.is_empty() => {
                if write_half.write_all(&data).await.is_err() {
                    pump.abort(); client.unregister_stream(stream_id).await;
                    anyhow::bail!("write error");
                }
                info!("attempt {}: got {} bytes for {}:{}", attempt, data.len(), dest_addr, dest_port);
                final_stream_id = stream_id;
                final_rx = Some(downstream_rx);
                final_pump = Some(pump);
                break;
            }
            _ => {
                pump.abort(); client.unregister_stream(stream_id).await;
                let _ = client.send_disconnect(stream_id).await;
                if attempt < max_attempts { info!("retry {}/{}", attempt, max_attempts); }
            }
        }
    }

    let mut downstream_rx = match final_rx {
        Some(rx) => rx,
        None => anyhow::bail!("no downstream after {} attempts", max_attempts),
    };
    let pump_handle = final_pump.unwrap();
    let stream_id = final_stream_id;

    // Upstream
    let initial_chunks = if initial_data.is_empty() { 0u32 } else {
        ((initial_data.len() + chunk_size - 1) / chunk_size) as u32
    };
    let upstream = tokio::spawn({
        let domain = client.domain.clone();
        let router = client.router.clone();
        let sid = client.session_id.clone();
        async move {
            let mut buf = vec![0u8; chunk_size * 8];
            let mut seq: u32 = 1 + initial_chunks;
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        for (i, chunk) in buf[..n].chunks(chunk_size).enumerate() {
                            let s = seq.wrapping_add(i as u32);
                            let cd = Bytes::copy_from_slice(chunk);
                            let r = router.clone(); let sid = sid.clone(); let dom = domain.clone();
                            tokio::spawn(async move {
                                let pkt = TunnelPacket::new_data(stream_id, s, cd);
                                let enc = pkt.encode();
                                let comp = compress::compress(&enc);
                                if let Ok(q) = build_query(&comp, &sid, &dom) {
                                    for _ in 0..5u8 {
                                        if r.send_via_good_paths(&q).await.is_ok() { break; }
                                    }
                                }
                            });
                        }
                        let nc = ((n + chunk_size - 1) / chunk_size) as u32;
                        seq = seq.wrapping_add(nc); if seq == 0 { seq = 1; }
                    }
                    Err(_) => break,
                }
            }
        }
    });

    // Downstream
    let downstream_handle = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(Duration::from_secs(8), downstream_rx.recv()).await {
                Ok(Some(data)) if !data.is_empty() => {
                    if write_half.write_all(&data).await.is_err() { break; }
                }
                _ => break,
            }
        }
    });

    tokio::select! { _ = upstream => {}, _ = downstream_handle => {} }
    pump_handle.abort();
    client.unregister_stream(stream_id).await;
    let _ = client.send_disconnect(stream_id).await;
    Ok(())
}

/// Streaming pump: INFLIGHT polls, process as they arrive.
/// Single session — all resolvers in one router, no reorder stall between groups.
async fn pump_task(
    session_id: String,
    domain: String,
    router: Arc<MultipathRouter>,
    streams: Arc<RwLock<HashMap<u16, StreamDownstream>>>,
    batch_id: Arc<AtomicU32>,
    batch_tracker: Arc<Mutex<BatchTracker>>,
    max_inflight: usize,
    _router_for_report: Arc<MultipathRouter>,
) {
    let mut poll_seq: u32 = 1_000_000;
    const POLL_TIMEOUT_MS: u64 = 500;

    let (result_tx, mut result_rx) = mpsc::channel::<PollResult>(max_inflight * 2);

    let mut total_sent: u64 = 0;
    let mut total_new: u64 = 0;
    let mut total_dup: u64 = 0;
    let mut total_empty: u64 = 0;
    let mut total_timeout: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut stats_start = std::time::Instant::now();
    let mut inflight: usize = 0;

    let spawn_poll = |seq: u32, batch_id: &Arc<AtomicU32>, router: &Arc<MultipathRouter>,
                      session_id: &str, domain: &str, tx: &mpsc::Sender<PollResult>| {
        let ack = batch_id.load(Ordering::Relaxed);
        let pkt = TunnelPacket {
            packet_type: PacketType::Poll, stream_id: 0, seq_num: seq,
            payload: Bytes::copy_from_slice(&ack.to_be_bytes()),
        };
        let compressed = compress::compress(&pkt.encode());
        let query = match build_query(&compressed, session_id, domain) {
            Ok(q) => q, Err(_) => return,
        };
        let router_c = router.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            let result = tokio::time::timeout(
                Duration::from_millis(POLL_TIMEOUT_MS),
                router_c.send_via_good_paths(&query),
            ).await;
            let _ = tx.send(PollResult { result, rtt: t0.elapsed().as_millis() as u64 }).await;
        });
    };

    // Seed
    for _ in 0..max_inflight {
        spawn_poll(poll_seq, &batch_id, &router, &session_id, &domain, &result_tx);
        poll_seq = poll_seq.wrapping_add(1);
        total_sent += 1;
        inflight += 1;
    }

    loop {
        let pr = match result_rx.recv().await { Some(pr) => pr, None => break };
        inflight -= 1;

        let mut got_new = false;
        match pr.result {
            Ok(Ok((resp, pid))) => {
                let has_data = resp.txt_data.as_ref().map_or(false, |d| !d.is_empty());
                if has_data {
                    let txt = resp.txt_data.as_ref().unwrap();
                    let recv_id = dispatch_downstream_with_batch(txt, &streams, &batch_tracker).await;
                    if recv_id > 0 {
                        got_new = true;
                        total_new += 1;
                        total_bytes += txt.len() as u64;
                        router.report_downstream_success(pid).await;
                        batch_id.store(batch_tracker.lock().await.ack, Ordering::Relaxed);
                    } else { total_dup += 1; }
                } else { total_empty += 1; }
            }
            Ok(Err(_)) => { total_empty += 1; }
            Err(_) => { total_timeout += 1; }
        }

        // No pacing — INFLIGHT cap limits RPS naturally.

        while inflight < max_inflight {
            spawn_poll(poll_seq, &batch_id, &router, &session_id, &domain, &result_tx);
            poll_seq = poll_seq.wrapping_add(1);
            total_sent += 1;
            inflight += 1;
        }

        if stats_start.elapsed() >= Duration::from_secs(2) {
            let elapsed = stats_start.elapsed().as_secs_f64();
            info!("PUMP [{:.1}s]: sent={} new={} dup={} empty={} timeout={} | ack={} | {:.0} RPS, {:.1} KB/s raw",
                elapsed, total_sent, total_new, total_dup, total_empty, total_timeout,
                batch_id.load(Ordering::Relaxed),
                total_sent as f64 / elapsed, total_bytes as f64 / elapsed / 1024.0);
            total_sent=0; total_new=0; total_dup=0; total_empty=0; total_timeout=0; total_bytes=0;
            stats_start = std::time::Instant::now();
        }
    }
}

struct PollResult {
    result: Result<Result<(DnsResponse, PathId), anyhow::Error>, tokio::time::error::Elapsed>,
    rtt: u64,
}

async fn dispatch_ds_packet(state: &mut StreamDownstream, seq: u32, data: Vec<u8>) {
    if seq > 0 && seq < state.next_ds_seq { return; }
    if seq == state.next_ds_seq {
        let _ = state.tx.send(data).await;
        state.next_ds_seq += 1;
        state.gap_since = None;
        while let Some(buffered) = state.reorder_buf.remove(&state.next_ds_seq) {
            let _ = state.tx.send(buffered).await;
            state.next_ds_seq += 1;
        }
    } else if seq > state.next_ds_seq {
        state.reorder_buf.insert(seq, data);
        if let Some(since) = state.gap_since {
            if since.elapsed() > Duration::from_secs(4) {
                if let Some((&next, _)) = state.reorder_buf.iter().next() {
                    warn!("gap timeout: skip seq {} to {}", state.next_ds_seq, next);
                    state.next_ds_seq = next;
                    state.gap_since = None;
                    while let Some(buffered) = state.reorder_buf.remove(&state.next_ds_seq) {
                        let _ = state.tx.send(buffered).await;
                        state.next_ds_seq += 1;
                    }
                }
            }
        } else { state.gap_since = Some(std::time::Instant::now()); }
    }
}

fn build_query(compressed: &[u8], session_id: &str, domain: &str) -> Result<sp_transport::DnsQuery> {
    let qname = dns_codec::encode_upstream(compressed, session_id, domain)?;
    build_dns_txt_query(&qname)
}

async fn dispatch_downstream_with_batch(
    txt_data: &[u8],
    streams: &Arc<RwLock<HashMap<u16, StreamDownstream>>>,
    tracker: &Mutex<BatchTracker>,
) -> u32 {
    let txt_str = match std::str::from_utf8(txt_data) { Ok(s) => s, Err(_) => return 0 };
    let decoded = match dns_codec::decode_downstream(txt_str) { Ok(d) => d, Err(_) => return 0 };
    if decoded.is_empty() { return 0; }
    let decompressed = compress::decompress(&decoded).unwrap_or(decoded);
    if decompressed.len() < 4 { return 0; }

    let batch_id = u32::from_be_bytes([decompressed[0], decompressed[1], decompressed[2], decompressed[3]]);
    let packet_data = &decompressed[4..];

    { let mut t = tracker.lock().await; if !t.receive(batch_id) { return 0; } }

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
    batch_id
}

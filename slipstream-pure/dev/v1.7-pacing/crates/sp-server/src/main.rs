use anyhow::Result;
use bytes::Bytes;
use clap::Parser;
use sp_common::crypto;
use sp_common::dns_codec;
use sp_common::protocol::{PacketType, TunnelPacket};
use sp_transport::build_dns_response;
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "sp-server", about = "Slipstream Pure DNS Tunnel Server")]
struct Args {
    #[arg(long, default_value = "tunnel.example.com")]
    domain: String,
    #[arg(long, default_value = "0.0.0.0:53")]
    dns_listen: String,
    #[arg(long, default_value = "server.crt")]
    cert: PathBuf,
    #[arg(long, default_value = "server.key")]
    key: PathBuf,
}

struct StreamHandle {
    tx: mpsc::Sender<Bytes>,
    next_seq: u32,
    reorder_buf: BTreeMap<u32, Bytes>,
}

struct Session {
    id: String,
    streams: tokio::sync::Mutex<HashMap<u16, StreamHandle>>,
    downstream_queue: tokio::sync::Mutex<Vec<Bytes>>,
    pending_downstream: tokio::sync::Mutex<Option<Vec<u8>>>,
    ds_batch_id: tokio::sync::Mutex<u32>,
}

type Sessions = Arc<RwLock<HashMap<String, Arc<Session>>>>;

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

    let (_certs, _key) = if args.cert.exists() && args.key.exists() {
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

    let sessions: Sessions = Arc::new(RwLock::new(HashMap::new()));
    let dns_addr: SocketAddr = args.dns_listen.parse()?;

    let udp_socket = UdpSocket::bind(dns_addr).await?;
    info!("DNS UDP listening on {}", dns_addr);

    let domain = Arc::new(args.domain.clone());
    let sessions_udp = sessions.clone();
    let domain_udp = domain.clone();
    let udp_socket = Arc::new(udp_socket);

    let _udp_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            match udp_socket.recv_from(&mut buf).await {
                Ok((n, src)) => {
                    let data = buf[..n].to_vec();
                    let sessions = sessions_udp.clone();
                    let domain = domain_udp.clone();
                    let udp = udp_socket.clone();
                    tokio::spawn(async move {
                        match handle_dns_query(&data, &domain, &sessions).await {
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
                Err(e) => error!("UDP recv error: {}", e),
            }
        }
    });

    let tcp_listener = tokio::net::TcpListener::bind(dns_addr).await?;
    info!("DNS TCP listening on {}", dns_addr);
    let sessions_tcp = sessions.clone();
    let domain_tcp = domain.clone();

    let _tcp_handle = tokio::spawn(async move {
        loop {
            match tcp_listener.accept().await {
                Ok((stream, src)) => {
                    let sessions = sessions_tcp.clone();
                    let domain = domain_tcp.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_tcp_client(stream, src, &domain, &sessions).await {
                            debug!("TCP client {} error: {}", src, e);
                        }
                    });
                }
                Err(e) => error!("TCP accept error: {}", e),
            }
        }
    });

    info!("Server running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");
    Ok(())
}

async fn handle_tcp_client(
    mut stream: tokio::net::TcpStream,
    src: SocketAddr,
    domain: &str,
    sessions: &Sessions,
) -> Result<()> {
    loop {
        let mut len_buf = [0u8; 2];
        if stream.read_exact(&mut len_buf).await.is_err() {
            return Ok(());
        }
        let len = u16::from_be_bytes(len_buf) as usize;
        if len == 0 || len > 65535 {
            return Ok(());
        }
        let mut query_buf = vec![0u8; len];
        stream.read_exact(&mut query_buf).await?;
        match handle_dns_query(&query_buf, domain, sessions).await {
            Ok(response) => {
                let resp_len = response.len() as u16;
                stream.write_all(&resp_len.to_be_bytes()).await?;
                stream.write_all(&response).await?;
                stream.flush().await?;
            }
            Err(e) => debug!("DNS query error from TCP {}: {}", src, e),
        }
    }
}

async fn handle_dns_query(
    query_wire: &[u8],
    domain: &str,
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

    if !qname_lower.ends_with(&format!(".{}.", domain))
        && !qname_lower.ends_with(&format!(".{}", domain))
    {
        return build_refused_response(query_wire);
    }

    let domain_suffix = format!(".{}", domain);
    let without_domain = qname_lower
        .trim_end_matches('.')
        .strip_suffix(&domain_suffix)
        .or_else(|| qname_lower.trim_end_matches('.').strip_suffix(domain))
        .ok_or_else(|| anyhow::anyhow!("cannot strip domain from {}", qname))?;

    let parts: Vec<&str> = without_domain.split('.').collect();
    if parts.is_empty() {
        return build_refused_response(query_wire);
    }

    let session_id = parts.last().unwrap().to_string();
    let data_labels = &parts[..parts.len() - 1];

    // Decode upstream data: try EDNS0 first, fall back to QNAME
    let upstream_data = if let Some(edns_data) = sp_transport::extract_edns0_tunnel_data(query_wire) {
        debug!("session {} EDNS0 upstream: {} bytes", session_id, edns_data.len());
        edns_data
    } else if data_labels.is_empty() {
        Vec::new()
    } else {
        let encoded_name = format!("{}.{}.{}", data_labels.join("."), session_id, domain);
        dns_codec::decode_upstream(&encoded_name, &session_id, domain)
            .unwrap_or_default()
    };

    let session = get_or_create_session(sessions, &session_id).await;

    let mut is_data_packet = false;
    let mut client_ack: u32 = 0;
    if !upstream_data.is_empty() {
        let decompressed = if upstream_data.len() >= 4 {
            let declared = u32::from_le_bytes([
                upstream_data[0], upstream_data[1],
                upstream_data[2], upstream_data[3],
            ]) as usize;
            if declared > 10_000 {
                upstream_data.clone()
            } else {
                sp_common::compress::decompress(&upstream_data)
                    .unwrap_or_else(|_| upstream_data.clone())
            }
        } else {
            upstream_data.clone()
        };

        if let Ok(packet) = TunnelPacket::decode(&decompressed) {
            is_data_packet = packet.packet_type == PacketType::Data;
            if packet.packet_type == PacketType::Poll && packet.payload.len() >= 4 {
                client_ack = u32::from_be_bytes([
                    packet.payload[0], packet.payload[1],
                    packet.payload[2], packet.payload[3],
                ]);
            }
            handle_tunnel_packet(packet, &session, sessions).await;
        }
    }

    let downstream = if is_data_packet {
        None
    } else {
        take_downstream_data(&session, client_ack).await
    };

    match downstream {
        Some(data) => {
            let compressed = sp_common::compress::compress(&data);
            let encoded = sp_common::dns_codec::encode_downstream(&compressed);
            let response = build_dns_response(query_wire, encoded.as_bytes())?;
            Ok(response.to_vec())
        }
        None => {
            let response = build_dns_response(query_wire, b"")?;
            Ok(response.to_vec())
        }
    }
}

async fn get_or_create_session(sessions: &Sessions, session_id: &str) -> Arc<Session> {
    {
        let sessions_read = sessions.read().await;
        if let Some(session) = sessions_read.get(session_id) {
            return session.clone();
        }
    }
    let mut sessions_write = sessions.write().await;
    if let Some(session) = sessions_write.get(session_id) {
        return session.clone();
    }
    info!("new session: {}", session_id);
    let session = Arc::new(Session {
        id: session_id.to_string(),
        streams: tokio::sync::Mutex::new(HashMap::new()),
        downstream_queue: tokio::sync::Mutex::new(Vec::new()),
        pending_downstream: tokio::sync::Mutex::new(None),
        ds_batch_id: tokio::sync::Mutex::new(0),
    });
    sessions_write.insert(session_id.to_string(), session.clone());
    session
}

async fn handle_tunnel_packet(packet: TunnelPacket, session: &Arc<Session>, sessions: &Sessions) {
    match packet.packet_type {
        PacketType::Connect => {
            if let Some((host, port)) = packet.parse_connect_payload() {
                let stream_id = packet.stream_id;
                {
                    let streams = session.streams.lock().await;
                    if streams.contains_key(&stream_id) {
                        return;
                    }
                }
                info!("session {} stream {} → {}:{}", session.id, stream_id, host, port);
                *session.pending_downstream.lock().await = None;
                *session.ds_batch_id.lock().await = 0;
                session.downstream_queue.lock().await.clear();

                let (tx, rx) = mpsc::channel::<Bytes>(256);
                session.streams.lock().await
                    .insert(stream_id, StreamHandle { tx, next_seq: 1, reorder_buf: BTreeMap::new() });

                let session_weak = Arc::downgrade(session);
                let sessions_ref = sessions.clone();
                let sid = session.id.clone();
                tokio::spawn(async move {
                    stream_forward_task(stream_id, &host, port, rx, session_weak).await;
                    if let Some(session) = sessions_ref.read().await.get(&sid) {
                        session.streams.lock().await.remove(&stream_id);
                    }
                });
            }
        }
        PacketType::Data => {
            let mut streams = session.streams.lock().await;
            let handle = match streams.get_mut(&packet.stream_id) {
                Some(h) => h,
                None => return,
            };
            let seq = packet.seq_num;
            if seq > 0 && seq < handle.next_seq { return; }
            if seq == handle.next_seq {
                let tx = handle.tx.clone();
                let mut to_send = vec![packet.payload];
                handle.next_seq += 1;
                while let Some(data) = handle.reorder_buf.remove(&handle.next_seq) {
                    handle.next_seq += 1;
                    to_send.push(data);
                }
                let stream_id = packet.stream_id;
                drop(streams);
                for data in to_send {
                    if tx.send(data).await.is_err() {
                        session.streams.lock().await.remove(&stream_id);
                        break;
                    }
                }
            } else if seq > handle.next_seq {
                handle.reorder_buf.insert(seq, packet.payload);
            }
        }
        PacketType::Poll => {}
        PacketType::Hello => {
            let mut queue = session.downstream_queue.lock().await;
            if queue.is_empty() {
                let echo = TunnelPacket::new_data(0, 0, Bytes::from_static(b"ok"));
                queue.push(echo.encode());
            }
        }
        PacketType::Bye => {
            session.streams.lock().await.clear();
            sessions.write().await.remove(&session.id);
        }
        PacketType::Disconnect => {
            session.streams.lock().await.remove(&packet.stream_id);
        }
    }
}

async fn stream_forward_task(
    stream_id: u16,
    host: &str,
    port: u16,
    mut upstream_rx: mpsc::Receiver<Bytes>,
    session: std::sync::Weak<Session>,
) {
    let addr = format!("{}:{}", host, port);

    let first_data = match upstream_rx.recv().await {
        Some(data) => data,
        None => return,
    };
    let mut initial_buf = first_data.to_vec();
    let buf_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
    while initial_buf.len() < 300 {
        match tokio::time::timeout_at(buf_deadline, upstream_rx.recv()).await {
            Ok(Some(data)) => initial_buf.extend_from_slice(&data),
            _ => break,
        }
    }

    let tcp = match TcpStream::connect(&addr).await {
        Ok(tcp) => tcp,
        Err(e) => {
            warn!("stream {} connect failed: {}", stream_id, e);
            if let Some(session) = session.upgrade() {
                let pkt = TunnelPacket::new_disconnect(stream_id);
                session.downstream_queue.lock().await.push(pkt.encode());
            }
            return;
        }
    };
    tcp.set_nodelay(true).ok();
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    if let Err(e) = tcp_write.write_all(&initial_buf).await {
        warn!("stream {} write error: {}", stream_id, e);
        if let Some(session) = session.upgrade() {
            let pkt = TunnelPacket::new_disconnect(stream_id);
            session.downstream_queue.lock().await.push(pkt.encode());
        }
        return;
    }

    let write_handle = tokio::spawn(async move {
        while let Some(data) = upstream_rx.recv().await {
            if tcp_write.write_all(&data).await.is_err() { break; }
        }
    });

    // Read downstream — 240 B chunks for safe DNS response size
    let mut buf = vec![0u8; 240];
    let mut ds_seq: u32 = 1;
    loop {
        match tcp_read.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if let Some(session) = session.upgrade() {
                    let packet = TunnelPacket::new_data(
                        stream_id, ds_seq, Bytes::copy_from_slice(&buf[..n]),
                    );
                    ds_seq += 1;
                    session.downstream_queue.lock().await.push(packet.encode());
                } else {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let abort = write_handle.abort_handle();
    if tokio::time::timeout(std::time::Duration::from_secs(5), write_handle)
        .await.is_err()
    {
        abort.abort();
    }

    if let Some(session) = session.upgrade() {
        let pkt = TunnelPacket::new_disconnect(stream_id);
        session.downstream_queue.lock().await.push(pkt.encode());
    }
}

/// ACK-based downstream delivery. Server keeps batch until client ACKs.
async fn take_downstream_data(session: &Session, client_ack: u32) -> Option<Vec<u8>> {
    const MAX_RAW: usize = 500;

    let mut batch_id = session.ds_batch_id.lock().await;
    let mut pending = session.pending_downstream.lock().await;

    if client_ack > 0 && client_ack == *batch_id {
        *pending = None;
    }

    if let Some(data) = pending.as_ref() {
        return Some(data.clone());
    }

    let mut queue = session.downstream_queue.lock().await;
    if queue.is_empty() {
        return None;
    }

    let mut total = 0;
    let mut taken = Vec::new();
    while let Some(data) = queue.first() {
        if total > 0 && total + data.len() > MAX_RAW {
            break;
        }
        total += data.len();
        taken.push(queue.remove(0));
    }

    if taken.is_empty() {
        return None;
    }

    *batch_id = batch_id.wrapping_add(1);

    let mut combined = Vec::with_capacity(total);
    for chunk in taken {
        combined.extend_from_slice(&chunk);
    }

    *pending = Some(combined.clone());
    Some(combined)
}

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

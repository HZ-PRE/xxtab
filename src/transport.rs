use crate::config::{Config, LatencyMode};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fastwebsockets::{Frame, OpCode, Payload, WebSocket};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, BufWriter},
    net::{TcpStream, UdpSocket},
    sync::{Notify, mpsc},
    time::{Instant, timeout},
};

pub const MAX_PACKET: usize = 2048;
const QUEUE: usize = 32;
const BATCH: usize = 16;
const MAX_QUEUE_AGE: Duration = Duration::from_millis(100);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
type Ws = WebSocket<TokioIo<hyper::upgrade::Upgraded>>;
type Connector = Arc<rustls::ClientConfig>;
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
#[derive(Default)]
pub struct Stats {
    pub tx: AtomicU64,
    pub rx: AtomicU64,
    pub dropped: AtomicU64,
    pub reconnects: AtomicU64,
    queue_full: AtomicU64,
    queue_expired: AtomicU64,
    queue_wait_us: AtomicU64,
    write_wait_us: AtomicU64,
    // Control-plane sample only: no locking for UDP/data frames.
    ws_probe: std::sync::Mutex<Option<(Instant, Duration)>>,
}
struct Packet {
    data: Vec<u8>,
    received: Instant,
}

// Owned by the current-thread relay. Storage is reused across packets and
// reconnects; no lock, allocation or task creation in the steady-state UDP path.
struct PacketQueue {
    pending: RefCell<VecDeque<Packet>>,
    free: RefCell<Vec<Vec<u8>>>,
    ready: Notify,
    interactive: bool,
    small_streak: Cell<usize>,
}

impl PacketQueue {
    fn new(interactive: bool) -> Self {
        Self {
            pending: RefCell::new(VecDeque::with_capacity(QUEUE)),
            free: RefCell::new(
                (0..=QUEUE)
                    .map(|_| Vec::with_capacity(MAX_PACKET))
                    .collect(),
            ),
            ready: Notify::new(),
            interactive,
            small_streak: Cell::new(0),
        }
    }

    // Returns false if ANY packet was dropped, including a bulk packet evicted
    // to admit a small packet. The caller counts one congestion drop either way.
    fn push(&self, bytes: &[u8]) -> bool {
        let mut pending = self.pending.borrow_mut();
        let dropped = pending.len() == QUEUE;
        let recycled = if dropped {
            if !self.interactive || bytes.len() > 256 {
                return false;
            }
            let Some(index) = pending.iter().position(|p| p.data.len() > 256) else {
                return false;
            };
            Some(pending.remove(index).unwrap().data)
        } else {
            None
        };
        // A cancelled write can discard one buffer on reconnect.
        let mut data = recycled
            .or_else(|| self.free.borrow_mut().pop())
            .unwrap_or_else(|| Vec::with_capacity(MAX_PACKET));
        data.clear();
        data.extend_from_slice(bytes);
        pending.push_back(Packet {
            data,
            received: Instant::now(),
        });
        self.ready.notify_one();
        !dropped
    }

    fn pop(&self) -> Option<Packet> {
        let mut pending = self.pending.borrow_mut();
        if !self.interactive {
            return pending.pop_front();
        }
        // Size is only a heuristic for encrypted ACK/RPC/DNS/handshake traffic.
        // At most four small packets ahead of a waiting bulk packet prevents starvation.
        let small = self.small_streak.get() < 4;
        let index = pending
            .iter()
            .position(|p| (p.data.len() <= 256) == small)
            .unwrap_or(0);
        let packet = pending.remove(index)?;
        self.small_streak.set(if packet.data.len() <= 256 {
            self.small_streak.get().saturating_add(1)
        } else {
            0
        });
        Some(packet)
    }

    async fn recv(&self) -> Packet {
        loop {
            if let Some(packet) = self.pop() {
                return packet;
            }
            self.ready.notified().await;
        }
    }

    fn recycle(&self, mut packet: Packet) {
        packet.data.clear();
        self.free.borrow_mut().push(packet.data);
    }

    fn discard(&self, stats: &Stats) {
        while let Some(packet) = self.pop() {
            self.recycle(packet);
            stats.dropped.fetch_add(1, Relaxed);
        }
    }
}

async fn write_batch<W: AsyncWrite + Unpin>(
    writer: &mut fastwebsockets::WebSocketWrite<W>,
    first: Packet,
    packets: &PacketQueue,
    stats: &Stats,
) -> Result<(), fastwebsockets::WebSocketError> {
    let started = Instant::now();
    let mut next = Some(first);
    let mut bytes = 0;
    for index in 0..BATCH {
        if index != 0 {
            next = packets.pop();
        }
        let Some(packet) = next.take() else {
            break;
        };
        let age = packet.received.elapsed();
        stats
            .queue_wait_us
            .fetch_max(age.as_micros() as u64, Relaxed);
        if age > MAX_QUEUE_AGE {
            stats.dropped.fetch_add(1, Relaxed);
            stats.queue_expired.fetch_add(1, Relaxed);
            packets.recycle(packet);
        } else {
            let result = writer
                .write_frame(Frame::binary(Payload::Borrowed(&packet.data)))
                .await;
            bytes += packet.data.len() as u64;
            packets.recycle(packet);
            result?;
        }
    }
    writer.flush().await?;
    stats
        .write_wait_us
        .fetch_max(started.elapsed().as_micros() as u64, Relaxed);
    stats.tx.fetch_add(bytes, Relaxed);
    Ok(())
}

pub async fn resolve(cfg: &Config) -> Result<SocketAddr> {
    let u = cfg.url()?;
    let port = u.port_or_known_default().context("missing server port")?;
    if let Some(ip) = cfg.server_ip {
        return Ok(SocketAddr::from((ip, port)));
    }
    timeout(
        IO_TIMEOUT,
        tokio::net::lookup_host((u.host_str().unwrap(), port)),
    )
    .await
    .context("DNS timeout")??
    .find(SocketAddr::is_ipv4)
    .context("server has no IPv4 address; set server_ip")
}

pub fn tls_connector(cfg: &Config) -> Result<Connector> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = &cfg.ca_file {
        let mut file =
            std::io::BufReader::new(std::fs::File::open(path).context("cannot open ca_file")?);
        let certs = rustls_pemfile::certs(&mut file).collect::<std::io::Result<Vec<_>>>()?;
        ensure!(!certs.is_empty(), "ca_file has no PEM certificates");
        for cert in certs {
            roots.add(cert)?;
        }
    }
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(tls))
}

fn request(cfg: &Config) -> Result<hyper::Request<http_body_util::Empty<hyper::body::Bytes>>> {
    let u = cfg.url()?;
    let (host, port) = cfg.destination()?;
    // The upstream server uses this JWT as metadata, without signature verification.
    // Access control uses TLS and the secret HTTP path, not this JWT signature.
    let payload = serde_json::json!({"id":"00000000-0000-4000-8000-000000000001", "p":{"Udp":{"timeout":null}}, "r":host, "rp":port});
    let jwt = format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload)?),
        URL_SAFE_NO_PAD.encode([0; 32])
    );
    let authority = match u.port() {
        Some(port) => format!("{}:{port}", u.host_str().unwrap()),
        None => u.host_str().unwrap().into(),
    };
    hyper::Request::builder()
        .method("GET")
        .uri(format!("/{}/events", cfg.path_prefix.trim_matches('/')))
        .header("Host", authority)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header(
            "Sec-WebSocket-Key",
            fastwebsockets::handshake::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Protocol",
            format!("v1, authorization.bearer.{jwt}"),
        )
        .body(http_body_util::Empty::new())
        .map_err(|_| anyhow::anyhow!("invalid WebSocket request"))
}

pub async fn connect(cfg: &Config, address: SocketAddr, tls: Connector) -> Result<Ws> {
    timeout(IO_TIMEOUT, async {
        let tcp = TcpStream::connect(address)
            .await
            .context("TCP connection failed")?;
        tcp.set_nodelay(true)?;
        if cfg.transport.latency_mode == LatencyMode::Interactive {
            let socket = socket2::SockRef::from(&tcp);
            #[cfg(target_os = "linux")]
            {
                // Limit unsent bytes, retaining the full window of bytes in flight.
                if socket.set_tcp_notsent_lowat(16 * 1024).is_err() {
                    crate::log!(
                        "TCP_NOTSENT_LOWAT unavailable; interactive mode uses packet priority only"
                    );
                }
            }
            #[cfg(windows)]
            socket
                .set_send_buffer_size(128 * 1024)
                .context("cannot set interactive TCP send buffer")?;
        }
        let u = cfg.url()?;
        let stream: Box<dyn Io> = if u.scheme() == "wss" {
            let name = rustls::pki_types::ServerName::try_from(u.host_str().unwrap().to_owned())?;
            let stream = tokio_rustls::TlsConnector::from(tls)
                .connect(name, tcp)
                .await
                .map_err(|_| {
                    anyhow::anyhow!("TLS verification/handshake failed; check hostname and ca_file")
                })?;
            Box::new(stream)
        } else {
            Box::new(tcp)
        };
        let (mut ws, _) =
            fastwebsockets::handshake::client(&TokioExecutor::new(), request(cfg)?, stream)
                .await
                .map_err(|e| match e {
                    fastwebsockets::WebSocketError::InvalidStatusCode(code) => {
                        anyhow::anyhow!("WebSocket upgrade rejected: HTTP {code}")
                    }
                    _ => anyhow::anyhow!("WebSocket handshake failed (network or protocol error)"),
                })?;
        ws.set_auto_apply_mask(false);
        ws.set_auto_pong(false);
        ws.set_auto_close(false);
        // fastwebsockets treats this limit as exclusive.
        ws.set_max_message_size(MAX_PACKET + 1);
        Ok(ws)
    })
    .await
    .context("connect/handshake timed out")?
}

async fn session(
    ws: Ws,
    udp: &UdpSocket,
    peer: &Cell<Option<SocketAddr>>,
    packets: &PacketQueue,
    stats: &Stats,
) -> Result<()> {
    let (mut reader, mut writer) = ws.split(|stream| {
        let (read, write) = tokio::io::split(stream);
        (read, BufWriter::with_capacity(16 * 1024, write))
    });
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<Vec<u8>>(4);
    let last_rx = Cell::new(Instant::now());
    let probe = Cell::new(None::<(u64, Instant)>);
    *stats.ws_probe.lock().unwrap() = None;
    let receive = async {
        let mut processed = 0;
        loop {
            let msg = reader
                .read_frame(&mut |_| std::future::ready(Ok::<(), std::io::Error>(())))
                .await
                .context("WebSocket read failed")?;
            last_rx.set(Instant::now());
            match msg.opcode {
                OpCode::Binary => {
                    ensure!(msg.fin, "fragmented UDP frame unsupported");
                    let dest = peer.get();
                    if let Some(dest) = dest {
                        timeout(IO_TIMEOUT, udp.send_to(&msg.payload, dest))
                            .await
                            .context("UDP send timeout")??;
                        stats.rx.fetch_add(msg.payload.len() as u64, Relaxed);
                    }
                }
                OpCode::Ping => {
                    let _ = ctrl_tx.try_send(msg.payload.to_vec());
                }
                OpCode::Pong => {
                    if let Some(rtt) = accept_probe(&probe, &msg.payload, Instant::now()) {
                        *stats.ws_probe.lock().unwrap() = Some((Instant::now(), rtt));
                    }
                }
                OpCode::Close => bail!("server closed tunnel"),
                _ => bail!("unexpected non-binary tunnel data"),
            }
            processed += 1;
            if processed == BATCH {
                processed = 0;
                tokio::task::yield_now().await;
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let send = async {
        let mut ping = tokio::time::interval(Duration::from_secs(15));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sequence = 0u64;
        loop {
            let frame = tokio::select! {
                _ = ping.tick() => {
                    ensure!(last_rx.get().elapsed() < Duration::from_secs(45), "server heartbeat timed out");
                    sequence = sequence.wrapping_add(1);
                    probe.set(Some((sequence, Instant::now())));
                    Frame::new(true, OpCode::Ping, None, Payload::Owned(sequence.to_be_bytes().to_vec()))
                }
                Some(payload) = ctrl_rx.recv() => Frame::pong(Payload::Owned(payload)),
                packet = packets.recv() => {
                    timeout(IO_TIMEOUT, write_batch(&mut writer, packet, packets, stats))
                        .await.context("WebSocket write timeout")??;
                    // Flush immediately when the queue empties. No batching timer
                    // delays interactive packets; yield prevents bursts starving RX.
                    tokio::task::yield_now().await;
                    continue;
                }
            };
            timeout(IO_TIMEOUT, async {
                writer.write_frame(frame).await?;
                writer.flush().await
            })
            .await
            .context("WebSocket write timeout")??;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    tokio::try_join!(receive, send)?;
    Ok(())
}

pub async fn run(
    cfg: &Config,
    address: SocketAddr,
    tls: Connector,
    first: Ws,
    udp: Arc<UdpSocket>,
    source: Option<SocketAddr>,
    stats: Arc<Stats>,
) -> Result<()> {
    let packets = PacketQueue::new(cfg.transport.latency_mode == LatencyMode::Interactive);
    crate::log!(
        "transport mode: {}; health log interval={}s",
        if packets.interactive {
            "interactive"
        } else {
            "balanced"
        },
        cfg.transport.diagnostics_interval_secs
    );
    let online = Cell::new(true);
    let peer = Cell::new(source);
    // These futures share one runtime thread, no per-packet task and no unbounded channel.
    let receive_udp = async {
        let mut buf = [0u8; MAX_PACKET + 1];
        let mut processed = 0;
        loop {
            // Tokio's normal IO budget exceeds this queue's capacity. Yield
            // earlier so a readable socket cannot fill it before TX gets polled.
            if processed == BATCH {
                processed = 0;
                tokio::task::yield_now().await;
            }
            processed += 1;
            let (len, addr) = match udp.recv_from(&mut buf).await {
                Ok(packet) => packet,
                // Windows reports WSAEMSGSIZE instead of returning a truncated datagram.
                Err(e) if cfg!(windows) && e.raw_os_error() == Some(10040) => {
                    stats.dropped.fetch_add(1, Relaxed);
                    continue;
                }
                Err(e) => return Err(e).context("local UDP receive failed"),
            };
            if !online.get() || len > MAX_PACKET {
                stats.dropped.fetch_add(1, Relaxed);
                continue;
            }
            {
                if peer.get().is_none() {
                    peer.set(Some(addr));
                }
                if peer.get() != Some(addr) {
                    stats.dropped.fetch_add(1, Relaxed);
                    continue;
                }
            }
            if !packets.push(&buf[..len]) {
                stats.dropped.fetch_add(1, Relaxed);
                stats.queue_full.fetch_add(1, Relaxed);
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let forward = async {
        let mut ws = first;
        let mut backoff = 1u64;
        loop {
            let started = Instant::now();
            let result = session(ws, &udp, &peer, &packets, &stats).await;
            online.set(false);
            *stats.ws_probe.lock().unwrap() = None;
            crate::logging::state(crate::logging::Status::Reconnecting);
            if started.elapsed() > Duration::from_secs(30) {
                backoff = 1;
            }
            crate::log!(
                "tunnel disconnected: {}; reconnecting",
                result.err().map(|e| e.to_string()).unwrap_or_default()
            );
            packets.discard(&stats);
            loop {
                // Small jitter prevents clients reconnecting in lockstep after an outage.
                let jitter = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_millis() as u64
                    % 251;
                tokio::time::sleep(Duration::from_millis(backoff * 1000 + jitter)).await;
                stats.reconnects.fetch_add(1, Relaxed);
                match connect(cfg, address, tls.clone()).await {
                    Ok(next) => {
                        ws = next;
                        online.set(true);
                        crate::logging::state(crate::logging::Status::Connected);
                        crate::log!("tunnel reconnected");
                        break;
                    }
                    Err(err) => {
                        crate::log!("reconnect failed: {err}");
                    }
                }
                backoff = (backoff * 2).min(30);
            }
            backoff = (backoff * 2).min(30);
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let diagnostics = async {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let (mut full, mut expired) = (0, 0);
        let mut reported = Instant::now();
        let (mut tx, mut rx) = (0, 0);
        loop {
            tick.tick().await;
            let next_full = stats.queue_full.load(Relaxed);
            let next_expired = stats.queue_expired.load(Relaxed);
            if next_full != full || next_expired != expired {
                crate::log!(
                    "transport congestion: queue full={} expired={} packets in last 5s",
                    next_full - full,
                    next_expired - expired
                );
            }
            (full, expired) = (next_full, next_expired);
            let interval = cfg.transport.diagnostics_interval_secs;
            if interval != 0 && reported.elapsed() >= Duration::from_secs(interval) {
                let elapsed = reported.elapsed().as_secs_f64();
                let next_tx = stats.tx.load(Relaxed);
                let next_rx = stats.rx.load(Relaxed);
                let probe = *stats.ws_probe.lock().unwrap();
                let queue_ms = stats.queue_wait_us.swap(0, Relaxed) as f64 / 1000.0;
                let write_ms = stats.write_wait_us.swap(0, Relaxed) as f64 / 1000.0;
                if next_tx != tx || next_rx != rx {
                    let rtt = probe
                        .map(|(received, rtt)| {
                            format!(
                                "{:.1}ms(sample_age={:.0}s)",
                                rtt.as_secs_f64() * 1000.0,
                                received.elapsed().as_secs_f64()
                            )
                        })
                        .unwrap_or_else(|| "unknown".into());
                    crate::log!(
                        "transport health: tx={:.1}KiB/s rx={:.1}KiB/s ws_rtt={} queue_max={:.1}ms write_max={:.1}ms",
                        (next_tx - tx) as f64 / elapsed / 1024.0,
                        (next_rx - rx) as f64 / elapsed / 1024.0,
                        rtt,
                        queue_ms,
                        write_ms
                    );
                }
                (tx, rx) = (next_tx, next_rx);
                reported = Instant::now();
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    tokio::try_join!(receive_udp, forward, diagnostics)?;
    Ok(())
}

fn accept_probe(
    probe: &Cell<Option<(u64, Instant)>>,
    payload: &[u8],
    now: Instant,
) -> Option<Duration> {
    let (sequence, sent) = probe.get()?;
    if payload != sequence.to_be_bytes() {
        return None;
    }
    probe.set(None);
    Some(now.saturating_duration_since(sent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_priority_is_bounded_and_preserves_bulk_progress() {
        let queue = PacketQueue::new(true);
        for index in 0..QUEUE {
            assert!(queue.push(&vec![index as u8; 1024]));
        }
        // Small control traffic can enter a full queue by replacing one bulk packet.
        assert!(!queue.push(b"control"));
        let first = queue.pop().unwrap();
        assert_eq!(first.data, b"control");
        queue.recycle(first);
        let first_bulk = queue.pop().unwrap();
        assert_eq!(first_bulk.data, vec![1; 1024]);
        queue.recycle(first_bulk);
        queue.discard(&Stats::default());
        assert!(queue.push(&[1; 1024]));
        for _ in 0..10 {
            assert!(queue.push(b"small"));
        }
        for _ in 0..4 {
            let packet = queue.pop().unwrap();
            assert_eq!(packet.data, b"small");
            queue.recycle(packet);
        }
        let packet = queue.pop().unwrap();
        assert_eq!(
            packet.data, [1; 1024],
            "bulk cannot be starved by small traffic"
        );
        queue.recycle(packet);
        queue.discard(&Stats::default());
        assert_eq!(queue.free.borrow().len(), QUEUE + 1);
    }

    #[test]
    fn probe_rejects_unrelated_and_duplicate_pongs() {
        let sent = Instant::now();
        let probe = Cell::new(Some((12, sent)));
        assert!(accept_probe(&probe, &11u64.to_be_bytes(), sent).is_none());
        assert!(accept_probe(&probe, b"", sent).is_none());
        assert_eq!(
            accept_probe(
                &probe,
                &12u64.to_be_bytes(),
                sent + Duration::from_millis(42)
            ),
            Some(Duration::from_millis(42))
        );
        assert!(accept_probe(&probe, &12u64.to_be_bytes(), sent).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queue_is_bounded_reuses_storage_and_survives_cancelled_wait() {
        let queue = PacketQueue::new(false);
        let original: std::collections::HashSet<_> =
            queue.free.borrow().iter().map(|v| v.as_ptr()).collect();
        assert!(
            timeout(Duration::from_millis(1), queue.recv())
                .await
                .is_err()
        );
        for cycle in 0..100 {
            for index in 0..QUEUE {
                assert!(queue.push(&[cycle, index as u8]));
            }
            assert!(!queue.push(b"overflow"));
            for index in 0..QUEUE {
                let packet = queue.recv().await;
                assert_eq!(packet.data, [cycle, index as u8]);
                assert!(original.contains(&packet.data.as_ptr()));
                queue.recycle(packet);
            }
        }
        assert!(queue.push(b"offline backlog"));
        let stats = Stats::default();
        queue.discard(&stats);
        assert!(queue.pop().is_none());
        assert_eq!(stats.dropped.load(Relaxed), 1);
        assert_eq!(queue.free.borrow().len(), QUEUE + 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn batches_flush_preserve_frames_and_discard_expired_packets() {
        use fastwebsockets::Role;
        let (client, server) = tokio::io::duplex(64 * 1024);
        let mut ws = WebSocket::after_handshake(client, Role::Client);
        ws.set_auto_apply_mask(false);
        let (_, mut writer) = ws.split(|io| {
            let (r, w) = tokio::io::split(io);
            (r, BufWriter::with_capacity(16 * 1024, w))
        });
        let mut reader = WebSocket::after_handshake(server, Role::Server);
        reader.set_auto_apply_mask(false);
        let queue = PacketQueue::new(false);
        let stats = Stats::default();
        for index in 0..QUEUE {
            assert!(queue.push(&vec![index as u8; 1280]));
        }
        queue.pending.borrow_mut()[0].received =
            Instant::now() - MAX_QUEUE_AGE - Duration::from_millis(1);
        write_batch(&mut writer, queue.recv().await, &queue, &stats)
            .await
            .unwrap();
        assert_eq!(queue.pending.borrow().len(), QUEUE - BATCH);
        for index in 1..BATCH {
            let frame = timeout(Duration::from_secs(1), reader.read_frame())
                .await
                .unwrap()
                .unwrap();
            assert!(frame.fin);
            assert_eq!(frame.opcode, OpCode::Binary);
            assert_eq!(&*frame.payload, vec![index as u8; 1280]);
        }
        assert_eq!(stats.queue_expired.load(Relaxed), 1);
        queue.discard(&stats);
        assert!(queue.push(b"interactive"));
        write_batch(&mut writer, queue.recv().await, &queue, &stats)
            .await
            .unwrap();
        let frame = timeout(Duration::from_secs(1), reader.read_frame())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&*frame.payload, b"interactive");
        assert_eq!(queue.free.borrow().len(), QUEUE + 1);
    }

    #[test]
    fn metadata_is_wstunnel_udp_without_idle_timeout() {
        let c: Config =
            toml::from_str("server='ws://localhost:8080'\nremote='127.0.0.1:51820'").unwrap();
        let r = request(&c).unwrap();
        assert_eq!(r.uri().path(), "/v1/events");
        let h = r.headers()["Sec-WebSocket-Protocol"].to_str().unwrap();
        let jwt = h.strip_prefix("v1, authorization.bearer.").unwrap();
        let payload: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(jwt.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payload["p"], serde_json::json!({"Udp":{"timeout":null}}));
        assert_eq!(payload["rp"], 51820);
    }
}

//! Requires an official wstunnel v10.7.1 executable and OpenSSL on PATH.
//! WSTUNNEL_BIN=/path/to/wstunnel cargo test --test interop -- --ignored --nocapture
use std::{
    io::Read,
    net::{TcpListener, TcpStream, UdpSocket},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(program: impl AsRef<std::ffi::OsStr>, args: &[String], log: &Path) -> Process {
    let mut cmd = Command::new(program);
    cmd.env_remove("NO_COLOR"); // upstream clap expects a boolean, some shells export NO_COLOR=1
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(log).unwrap());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    Process(cmd.spawn().unwrap())
}
fn free_tcp() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn free_udp() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn wait_server(port: u16) {
    let until = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < until, "server did not listen");
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn wait_ready(process: &mut Process, log: &Path) {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let text = std::fs::read_to_string(log).unwrap();
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "client exited: {text}"
        );
        if text.contains("tunnel ready") {
            return;
        }
        assert!(Instant::now() < until, "client not ready: {text}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
struct Echo {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    received: Arc<AtomicU64>,
}
impl Echo {
    fn new() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket2::SockRef::from(&socket)
            .set_recv_buffer_size(1024 * 1024)
            .unwrap();
        let port = socket.local_addr().unwrap().port();
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let received = Arc::new(AtomicU64::new(0));
        let count = received.clone();
        let thread = std::thread::spawn(move || {
            let mut data = [0u8; 65536];
            while !stopped.load(Relaxed) {
                if let Ok((n, from)) = socket.recv_from(&mut data) {
                    count.fetch_add(1, Relaxed);
                    socket.send_to(&data[..n], from).unwrap();
                }
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
            received,
        }
    }
}
impl Drop for Echo {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn roundtrip(socket: &UdpSocket, port: u16, data: &[u8]) {
    socket.send_to(data, ("127.0.0.1", port)).unwrap();
    let mut buf = [0u8; 4096];
    let n = socket
        .recv(&mut buf)
        .unwrap_or_else(|e| panic!("UDP response timeout for {} byte packet: {e}", data.len()));
    assert_eq!(&buf[..n], data, "datagram boundaries/content changed");
}

fn run_case(tls: bool, load: bool) {
    run_case_mode(tls, load, "balanced");
}

fn run_case_mode(tls: bool, load: bool, mode: &str) {
    let upstream =
        std::env::var_os("WSTUNNEL_BIN").expect("set WSTUNNEL_BIN to official wstunnel binary");
    let dir = tempfile::tempdir().unwrap();
    let echo = Echo::new();
    let server_port = free_tcp();
    let local_port = free_udp();
    let scheme = if tls { "wss" } else { "ws" };
    let prefix = "interop-secret-do-not-log";
    let mut args = vec![
        "server".into(),
        "--restrict-to".into(),
        format!("127.0.0.1:{}", echo.port),
        "--restrict-http-upgrade-path-prefix".into(),
        prefix.into(),
        "--websocket-ping-frequency".into(),
        "1s".into(),
        format!("{scheme}://127.0.0.1:{server_port}"),
    ];
    if tls {
        let cert = dir.path().join("ca.pem");
        let key = dir.path().join("key.pem");
        let status = Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost",
                "-addext",
                "basicConstraints=critical,CA:FALSE",
                "-keyout",
            ])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        args.extend([
            "--tls-certificate".into(),
            cert.to_string_lossy().into_owned(),
            "--tls-private-key".into(),
            key.to_string_lossy().into_owned(),
        ]);
    }
    let server_log = dir.path().join("server.log");
    let mut server = spawn(&upstream, &args, &server_log);
    wait_server(server_port);
    let configuration = format!(
        "server = '{scheme}://localhost:{server_port}'\nserver_ip = '127.0.0.1'\npath_prefix = '{prefix}'\nremote = '127.0.0.1:{}'\nlisten = '127.0.0.1:{local_port}'\n{}[transport]\nlatency_mode='{mode}'\ndiagnostics_interval_secs=5\n",
        echo.port,
        if tls { "ca_file = 'ca.pem'\n" } else { "" }
    );
    let config = dir.path().join("client.toml");
    std::fs::write(&config, &configuration).unwrap();
    let client_log = dir.path().join("client.log");
    let exe = std::env::var_os("XXTAB_BIN").unwrap_or_else(|| env!("CARGO_BIN_EXE_xxtab").into());
    let client_args = vec!["relay".into(), config.to_string_lossy().into_owned()];
    let mut client = spawn(&exe, &client_args, &client_log);
    wait_ready(&mut client, &client_log);
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    if load {
        load_test(&socket, local_port, &mut client, scheme, &echo);
        let log = std::fs::read_to_string(&client_log).unwrap();
        for line in log.lines().filter(|line| line.contains("congestion")) {
            println!("LOAD {line}");
        }
        return;
    }
    let begin = Instant::now();
    for i in 0..1000usize {
        let len = [1, 32, 148, 1024, 1432, 2048][i % 6];
        roundtrip(&socket, local_port, &vec![(i % 251) as u8; len]);
    }
    println!(
        "{scheme}: 1000 verified UDP roundtrips in {:?}",
        begin.elapsed()
    );
    #[cfg(windows)]
    {
        let metrics = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &format!("Get-Process -Id {} | Select-Object WorkingSet64,PrivateMemorySize64,@{{n='Threads';e={{$_.Threads.Count}}}} | ConvertTo-Json -Compress", client.0.id())]).output().unwrap();
        println!(
            "{scheme}: client process metrics {}",
            String::from_utf8_lossy(&metrics.stdout)
        );
    }
    // Foreign local senders cannot steal the pinned return address.
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    stranger
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    stranger
        .send_to(b"not-the-peer", ("127.0.0.1", local_port))
        .unwrap();
    assert!(stranger.recv(&mut [0u8; 100]).is_err());
    // Oversized UDP must be dropped without terminating the client (including WSAEMSGSIZE).
    socket
        .send_to(&[0u8; 4096], ("127.0.0.1", local_port))
        .unwrap();
    roundtrip(&socket, local_port, b"after-oversize");
    // Exercise server Ping/Pong during an idle period.
    std::thread::sleep(Duration::from_secs(4));
    roundtrip(&socket, local_port, b"after-idle");
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    std::thread::sleep(Duration::from_millis(400));
    for _ in 0..1000 {
        socket
            .send_to(b"discard-during-outage", ("127.0.0.1", local_port))
            .unwrap();
    }
    server = spawn(&upstream, &args, &server_log);
    wait_server(server_port);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !std::fs::read_to_string(&client_log)
        .unwrap()
        .contains("tunnel reconnected")
    {
        assert!(
            Instant::now() < deadline,
            "reconnect timeout: {}",
            std::fs::read_to_string(&client_log).unwrap()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    roundtrip(&socket, local_port, b"after-reconnect");
    assert!(client.0.try_wait().unwrap().is_none());
    drop(client);
    drop(server);
    let mut log = String::new();
    std::fs::File::open(&client_log)
        .unwrap()
        .read_to_string(&mut log)
        .unwrap();
    assert!(!log.contains(prefix), "path secret leaked into logs");
    if tls {
        // Default trust must reject the private self-signed certificate.
        let untrusted = dir.path().join("untrusted.toml");
        std::fs::write(
            &untrusted,
            configuration.replace("ca_file = 'ca.pem'\n", ""),
        )
        .unwrap();
        let _server = spawn(&upstream, &args, &server_log);
        wait_server(server_port);
        let mut untrusted_client = spawn(
            &exe,
            &["relay".into(), untrusted.to_string_lossy().into_owned()],
            &client_log,
        );
        let status = untrusted_client.0.wait().unwrap();
        assert!(!status.success(), "untrusted TLS certificate accepted");
        assert!(
            std::fs::read_to_string(&client_log)
                .unwrap()
                .contains("TLS")
        );
    }
}

#[test]
#[ignore = "requires official wstunnel executable"]
fn official_ws_udp_and_reconnect() {
    run_case(false, false);
}
#[test]
#[ignore = "requires official wstunnel executable and openssl"]
fn official_wss_udp_tls_and_reconnect() {
    run_case(true, false);
}

#[test]
#[ignore = "requires official wstunnel executable and openssl"]
fn official_interactive_ws_wss() {
    run_case_mode(false, false, "interactive");
    run_case_mode(true, false, "interactive");
}

// Finite bursts include an interactive (one packet) case. Count missing responses
// instead of silently retransmitting them; assert every received datagram verbatim.
fn load_test(socket: &UdpSocket, port: u16, client: &mut Process, scheme: &str, echo: &Echo) {
    // Keep the load generator's own receive queue out of the bottleneck.
    socket2::SockRef::from(socket)
        .set_recv_buffer_size(1024 * 1024)
        .unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(30)))
        .unwrap();
    roundtrip(socket, port, b"warmup");
    for burst in [1usize, 32, 128] {
        let rounds = if burst == 128 { 200 } else { 5000 };
        let mut data = [0x5au8; 1280];
        let mut response = [0u8; 2049];
        let mut latency = Vec::with_capacity(rounds * burst);
        let mut lost = 0;
        let cpu_start = cpu_seconds(client);
        let echo_start = echo.received.load(Relaxed);
        let start = Instant::now();
        for round in 0..rounds {
            let sent = Instant::now();
            for index in 0..burst {
                data[..8].copy_from_slice(&((round * burst + index) as u64).to_le_bytes());
                socket.send_to(&data, ("127.0.0.1", port)).unwrap();
            }
            let mut seen = vec![false; burst];
            let mut received = 0;
            while received < burst {
                match socket.recv(&mut response) {
                    Ok(n) => {
                        assert_eq!(n, data.len());
                        assert!(response[8..n].iter().all(|v| *v == 0x5a));
                        let id = u64::from_le_bytes(response[..8].try_into().unwrap()) as usize;
                        if id < round * burst {
                            continue;
                        } // late, already counted as lost
                        assert!(id < (round + 1) * burst);
                        let index = id % burst;
                        assert!(!seen[index], "duplicate UDP datagram");
                        seen[index] = true;
                        received += 1;
                        latency.push(sent.elapsed().as_secs_f64() * 1000.0);
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(e) => panic!("load receive: {e}"),
                }
            }
            lost += burst - received;
        }
        let elapsed = start.elapsed().as_secs_f64();
        let cpu = cpu_seconds(client) - cpu_start;
        latency.sort_by(f64::total_cmp);
        assert!(!latency.is_empty());
        println!(
            "LOAD {}",
            serde_json::json!({
                "scheme": scheme, "burst": burst, "bytes": data.len(),
                "sent": rounds * burst, "lost": lost, "seconds": elapsed,
                "mbps": latency.len() as f64 * data.len() as f64 * 8.0 / elapsed / 1e6,
                "p50_ms": latency[latency.len() / 2],
                "p99_ms": latency[(latency.len() - 1) * 99 / 100],
            "client_cpu_seconds": cpu,
            "echo_received": echo.received.load(Relaxed) - echo_start,
            })
        );
        assert!(client.0.try_wait().unwrap().is_none());
    }
    #[cfg(windows)]
    {
        let metrics = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &format!("Get-Process -Id {} | Select-Object WorkingSet64,PrivateMemorySize64 | ConvertTo-Json -Compress", client.0.id())]).output().unwrap();
        println!("LOAD memory {}", String::from_utf8_lossy(&metrics.stdout));
    }
}

fn cpu_seconds(client: &Process) -> f64 {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};
        let mut times = [FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }; 4];
        let [created, exited, kernel, user] = &mut times;
        assert_ne!(
            unsafe { GetProcessTimes(client.0.as_raw_handle(), created, exited, kernel, user) },
            0
        );
        let ticks = |t: &FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
        (ticks(kernel) + ticks(user)) as f64 / 1e7
    }
    #[cfg(not(windows))]
    {
        let _ = client;
        0.0
    }
}

#[test]
#[ignore = "load benchmark requires official wstunnel and openssl"]
fn official_load() {
    run_case(false, true);
    run_case(true, true);
}

#[test]
fn check_rejects_invalid_config_without_exposing_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path: PathBuf = dir.path().join("bad.toml");
    std::fs::write(
        &path,
        "server = 'wss://example.com'\npath_prefix = 'VERY_SECRET'\nunknown = true",
    )
    .unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_xxtab"))
        .arg("check")
        .arg(path)
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(!String::from_utf8_lossy(&o.stderr).contains("VERY_SECRET"));
}

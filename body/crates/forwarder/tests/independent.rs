//! Independent tests for the forwarder (Demo-0 step 3a).
//!
//! Written by a reviewer, not the author. Uses only the crate's public API
//! and the built binary. Attacks the weak spots listed in the task brief.

use praxion_forwarder::{Config, Forwarder};
use praxion_tee::{Kind, Reader, Record, HEADER};

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- helpers

fn test_config(upstream: SocketAddr, tee_queue_bytes: usize, timeout: Duration) -> Config {
    Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        upstream,
        tee_listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        stats: None,
        tee_queue_bytes,
        protocol: 777,
        first_frame_timeout: timeout,
    }
}

fn start_forwarder(config: Config) -> (SocketAddr, SocketAddr) {
    let forwarder = Forwarder::bind(config).expect("forwarder binds");
    let game = forwarder.local_addr().expect("game address");
    let tee = forwarder.tee_local_addr().expect("tee address");
    thread::spawn(move || forwarder.run());
    (game, tee)
}

fn varint(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn handshake(protocol: i32, intent: i32) -> Vec<u8> {
    let host = b"localhost";
    let mut payload = varint(0);
    payload.extend(varint(protocol));
    payload.extend(varint(host.len() as i32));
    payload.extend_from_slice(host);
    payload.extend_from_slice(&25565u16.to_be_bytes());
    payload.extend(varint(intent));
    let mut frame = varint(payload.len() as i32);
    frame.extend(payload);
    frame
}

fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = vec![0u8; len];
    let mut i = 0;
    while i < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let value = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let take = (len - i).min(8);
        out[i..i + take].copy_from_slice(&value.to_le_bytes()[..take]);
        i += take;
    }
    out
}

fn spawn_sink() -> (SocketAddr, Arc<AtomicU64>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let received = Arc::new(AtomicU64::new(0));
    let counter = received.clone();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let _ = stream.set_nodelay(true);
            let counter = counter.clone();
            thread::spawn(move || {
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            counter.fetch_add(n as u64, Ordering::Relaxed);
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    });
    (addr, received)
}

fn spawn_echo() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let _ = stream.set_nodelay(true);
            thread::spawn(move || {
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if stream.write_all(&buffer[..n]).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = stream.shutdown(Shutdown::Write);
            });
        }
    });
    addr
}

struct Probe {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    data: Arc<Mutex<Vec<u8>>>,
}

fn spawn_probe() -> Probe {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let data = Arc::new(Mutex::new(Vec::new()));
    let counter = connections.clone();
    let collected = data.clone();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            counter.fetch_add(1, Ordering::Relaxed);
            let collected = collected.clone();
            thread::spawn(move || {
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => collected.lock().unwrap().extend_from_slice(&buffer[..n]),
                        Err(_) => break,
                    }
                }
            });
        }
    });
    Probe {
        addr,
        connections,
        data,
    }
}

fn connect_tee(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("tee subscriber connects");
    stream.set_nodelay(true).unwrap();
    let mut header = [0u8; 8];
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(n) = stream.peek(&mut header) {
            if n >= 8 {
                assert_eq!(&header, HEADER, "tee stream header");
                return stream;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("the tee never wrote its header");
}

fn collect(
    stream: TcpStream,
    timeout: Duration,
    done: Box<dyn Fn(&[Record]) -> bool + Send>,
) -> JoinHandle<Vec<Record>> {
    thread::spawn(move || {
        stream.set_read_timeout(Some(timeout)).unwrap();
        let mut reader = Reader::new(stream).expect("tee header");
        let mut records = Vec::new();
        while let Some(item) = reader.next() {
            match item {
                Ok(record) => {
                    let closed = record.kind == Kind::Closed;
                    records.push(record);
                    if closed || done(&records) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        records
    })
}

fn collect_multi(
    stream: TcpStream,
    timeout: Duration,
    done: Box<dyn Fn(&[Record]) -> bool + Send>,
) -> JoinHandle<Vec<Record>> {
    thread::spawn(move || {
        stream.set_read_timeout(Some(timeout)).unwrap();
        let mut reader = Reader::new(stream).expect("tee header");
        let mut records = Vec::new();
        while let Some(item) = reader.next() {
            match item {
                Ok(record) => {
                    records.push(record);
                    if done(&records) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        records
    })
}

fn collected_bytes(records: &[Record], kind: Kind) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        if record.kind == kind {
            out.extend_from_slice(&record.data);
        }
    }
    out
}

fn dropped_bytes(records: &[Record]) -> u64 {
    records
        .iter()
        .filter_map(|record| match record.kind {
            Kind::Gap { dropped_bytes } => Some(dropped_bytes),
            _ => None,
        })
        .sum()
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    condition()
}

#[cfg(windows)]
fn process_threads() -> usize {
    let pid = std::process::id();
    let script = format!("(Get-Process -Id {}).Threads.Count", pid);
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .expect("powershell");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

/// `ps -M` prints a header and one line per thread.
#[cfg(target_os = "macos")]
fn process_threads() -> usize {
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-M", "-p", &pid])
        .output()
        .expect("ps");
    let lines = String::from_utf8_lossy(&output.stdout).lines().count();
    assert!(lines > 1, "ps -M listed no threads");
    lines - 1
}

#[cfg(target_os = "linux")]
fn process_threads() -> usize {
    std::fs::read_dir("/proc/self/task").expect("/proc/self/task").count()
}

/// Open file descriptors; sockets count here like handles do on Windows.
#[cfg(unix)]
fn process_handles() -> usize {
    std::fs::read_dir("/dev/fd").expect("/dev/fd").count()
}

#[cfg(windows)]
fn process_handles() -> usize {
    let pid = std::process::id();
    let script = format!("(Get-Process -Id {}).HandleCount", pid);
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .expect("powershell");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

struct SlowReader {
    inner: TcpStream,
}

impl Read for SlowReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let cap = buf.len().min(1024);
        let n = self.inner.read(&mut buf[..cap])?;
        if n > 0 {
            thread::sleep(Duration::from_millis(10));
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn independent_many_concurrent_connections_tee_reconstructs_per_connection() {
    const N: usize = 20;
    const PAYLOAD: usize = 64 * 1024;
    let upstream = spawn_echo();
    let (game, tee) = start_forwarder(test_config(upstream, 64 * 1024 * 1024, Duration::from_secs(10)));

    let subscriber = connect_tee(tee);
    let collector = collect_multi(
        subscriber,
        Duration::from_secs(60),
        Box::new(|records| records.iter().filter(|r| r.kind == Kind::Closed).count() >= N),
    );

    let mut clients: Vec<TcpStream> = Vec::with_capacity(N);
    for i in 0..N {
        let mut client = TcpStream::connect(game).unwrap();
        client.set_nodelay(true).unwrap();
        let mut data = handshake(777, 2);
        data.extend(random_bytes(0x1000 + i as u64, PAYLOAD));
        client.write_all(&data).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        clients.push(client);
    }

    for client in clients.iter_mut() {
        let mut buf = [0u8; 4096];
        // drain until EOF
        loop {
            match client.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    let records = collector.join().unwrap();

    // conn ids unique and cover 0..N
    let mut ids: Vec<u32> = records.iter().map(|r| r.conn_id).collect();
    ids.sort();
    ids.dedup();
    for i in 0..N as u32 {
        assert!(ids.contains(&i), "missing conn id {}", i);
    }
    let opened_count = records.iter().filter(|r| r.kind == Kind::Opened).count();
    assert_eq!(opened_count, N, "Opened count");

    for conn in 0..N as u32 {
        let sb: Vec<u8> = records
            .iter()
            .filter(|r| r.conn_id == conn && r.kind == Kind::Serverbound)
            .flat_map(|r| r.data.clone())
            .collect();
        let cb: Vec<u8> = records
            .iter()
            .filter(|r| r.conn_id == conn && r.kind == Kind::Clientbound)
            .flat_map(|r| r.data.clone())
            .collect();
        assert_eq!(sb.len(), handshake(777, 2).len() + PAYLOAD, "conn {} sb len", conn);
        assert_eq!(sb, cb, "conn {} directions differ", conn);
        assert!(
            records.iter().any(|r| r.conn_id == conn && r.kind == Kind::Closed),
            "conn {} missing Closed",
            conn
        );
    }
}

#[test]
fn independent_slow_subscriber_does_not_slow_forwarding_and_gaps_add_up() {
    const PAYLOAD: usize = 20 * 1024 * 1024;
    let (upstream, received) = spawn_sink();
    let (game, tee) = start_forwarder(test_config(upstream, 128 * 1024, Duration::from_secs(10)));

    let subscriber = connect_tee(tee);
    let subscriber_for_reader = subscriber.try_clone().unwrap();
    subscriber_for_reader
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();

    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = captured.clone();
    let reader_thread = thread::spawn(move || {
        let slow = SlowReader {
            inner: subscriber_for_reader,
        };
        let mut reader = Reader::new(slow).expect("tee header");
        let mut records = Vec::new();
        for item in reader.by_ref() {
            match item {
                Ok(record) => {
                    let closed = record.kind == Kind::Closed;
                    records.push(record);
                    if closed {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        *captured_clone.lock().unwrap() = records;
    });

    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();
    let mut sent = handshake(777, 2);
    sent.extend(random_bytes(0x5EED, PAYLOAD));

    let start = Instant::now();
    client.write_all(&sent).expect("client sends while tee is stalled");
    let write_time = start.elapsed();
    assert!(
        write_time < Duration::from_secs(5),
        "forwarding took {write_time:?} with a stalled subscriber"
    );
    assert!(
        wait_until(Duration::from_secs(30), || {
            received.load(Ordering::Relaxed) as usize == sent.len()
        }),
        "the server did not receive the payload"
    );

    client.shutdown(Shutdown::Write).unwrap();
    reader_thread.join().unwrap();

    let records = captured.lock().unwrap().clone();
    let teed = collected_bytes(&records, Kind::Serverbound).len() as u64;
    let gaps = dropped_bytes(&records);
    assert!(gaps > 0, "a stalled subscriber must produce gaps");
    assert_eq!(
        teed + gaps,
        sent.len() as u64,
        "bytes forwarded = bytes teed + bytes in gaps"
    );
    assert!(records.iter().any(|r| r.kind == Kind::Closed));
}

#[test]
fn independent_subscriber_garbage_half_close_reset_does_not_break_forwarding() {
    let (upstream, received) = spawn_sink();
    let (game, tee) = start_forwarder(test_config(upstream, 1024 * 1024, Duration::from_secs(10)));

    // Misbehaving subscriber: sends garbage, half-closes, then is dropped (reset).
    let mut sub = connect_tee(tee);
    let _ = sub.write_all(b"garbage-bytes");
    let _ = sub.shutdown(Shutdown::Write);
    drop(sub);

    thread::sleep(Duration::from_millis(300));

    let sub2 = connect_tee(tee);
    let collector = collect(sub2, Duration::from_secs(10), Box::new(|_| false));

    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();
    let mut head = handshake(777, 2);
    head.extend_from_slice(b"after-reset");
    client.write_all(&head).unwrap();
    client.shutdown(Shutdown::Write).unwrap();

    assert!(wait_until(Duration::from_secs(5), || {
        received.load(Ordering::Relaxed) as usize == head.len()
    }));

    let records = collector.join().unwrap();
    assert!(
        records
            .iter()
            .any(|r| r.kind == Kind::Serverbound && r.data.windows(b"after-reset".len()).any(|w| w == b"after-reset")),
        "live records survive a reset subscriber"
    );
}

#[test]
fn independent_upstream_refused_repeatedly_does_not_leak() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let upstream = listener.local_addr().unwrap();
    drop(listener);

    let (game, _tee) = start_forwarder(test_config(upstream, 1024 * 1024, Duration::from_secs(10)));

    let before_threads = process_threads();
    let before_handles = process_handles();

    for _ in 0..200 {
        if let Ok(mut client) = TcpStream::connect(game) {
            let _ = client.write_all(&handshake(777, 2));
            // Do not wait for the refusal; let the forwarder's threads run concurrently.
            let _ = client.shutdown(Shutdown::Both);
        }
    }
    // Give the refused upstream connections time to finish (Windows connect ~2s).
    thread::sleep(Duration::from_secs(10));

    thread::sleep(Duration::from_secs(1));
    let after_threads = process_threads();
    let after_handles = process_handles();

    assert!(
        after_threads <= before_threads + 10,
        "thread count grew: {} -> {}",
        before_threads,
        after_threads
    );
    assert!(
        after_handles <= before_handles + 50,
        "handle count grew: {} -> {}",
        before_handles,
        after_handles
    );
}

#[test]
fn independent_client_closes_during_gate_wait() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(probe.addr, 1024 * 1024, Duration::from_secs(10)));
    let mut client = TcpStream::connect(game).unwrap();
    client.write_all(&[0x00]).unwrap();
    drop(client);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(probe.connections.load(Ordering::Relaxed), 0);
}

#[test]
fn independent_gate_ten_second_timeout() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(probe.addr, 1024 * 1024, Duration::from_secs(10)));
    let mut client = TcpStream::connect(game).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(12))).unwrap();
    let start = Instant::now();
    let mut buffer = [0u8; 64];
    loop {
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_secs(9) && elapsed <= Duration::from_secs(12),
        "gate timeout fired after {elapsed:?}"
    );
    assert_eq!(probe.connections.load(Ordering::Relaxed), 0);
}

#[test]
fn independent_status_ping_protocol_776_passes() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(probe.addr, 1024 * 1024, Duration::from_secs(10)));
    let frame = handshake(776, 1);
    let mut client = TcpStream::connect(game).unwrap();
    client.write_all(&frame).unwrap();
    assert!(wait_until(Duration::from_secs(5), || {
        probe.connections.load(Ordering::Relaxed) == 1
    }));
    assert!(wait_until(Duration::from_secs(5), || {
        probe.data.lock().unwrap().as_slice() == frame.as_slice()
    }));
}

#[test]
fn independent_login_with_trailing_packet_forwards_extra_bytes_unchanged() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(probe.addr, 1024 * 1024, Duration::from_secs(10)));
    let frame = handshake(777, 2);
    let extra = random_bytes(0xABC, 100);
    let mut combined = frame.clone();
    combined.extend_from_slice(&extra);

    let mut client = TcpStream::connect(game).unwrap();
    client.write_all(&combined).unwrap();

    assert!(wait_until(Duration::from_secs(5), || {
        probe.data.lock().unwrap().as_slice() == combined.as_slice()
    }));
    assert_eq!(probe.connections.load(Ordering::Relaxed), 1);
}

#[test]
fn independent_binary_refuses_non_loopback_listen() {
    let bin = env!("CARGO_BIN_EXE_praxion-forwarder");
    let output = Command::new(bin)
        .args(["--listen", "0.0.0.0:12345"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("loopback"), "stderr: {}", stderr);

    let output = Command::new(bin)
        .args(["--tee-listen", "192.168.1.1:25600"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("loopback"), "stderr: {}", stderr);
}

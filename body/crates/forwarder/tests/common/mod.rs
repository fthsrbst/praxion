//! Fixtures shared by the forwarder's unit and integration tests.
//!
//! Only the crate's public API is used, so this file compiles both as the
//! integration-test module `tests/common/mod.rs` and through the `#[path]`
//! include in `src/tests.rs`. The independent suites keep their own assertions;
//! only setup and fake upstreams live here.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
#[cfg(any(windows, target_os = "macos"))]
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use praxion_tee::{HEADER, Kind, Reader, Record};

use super::{Config, Forwarder};

pub fn test_config(upstream: SocketAddr, tee_queue_bytes: usize, timeout: Duration) -> Config {
    Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        upstream,
        tee_listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        tee_queue_bytes,
        protocol: 777,
        first_frame_timeout: timeout,
        ..Config::default()
    }
}

pub fn start_forwarder(config: Config) -> (SocketAddr, SocketAddr) {
    let forwarder = Forwarder::bind(config).expect("forwarder binds");
    let game = forwarder.local_addr().expect("game address");
    let tee = forwarder.tee_local_addr().expect("tee address");
    thread::spawn(move || forwarder.run());
    (game, tee)
}

pub fn varint(value: i32) -> Vec<u8> {
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

pub fn handshake(protocol: i32, intent: i32) -> Vec<u8> {
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

/// Deterministic pseudo-random bytes: xorshift64*.
pub fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
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

/// Connect a tee subscriber and wait until the forwarder has written the stream header, so a
/// subsequently opened game connection cannot be missed.
pub fn connect_tee(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("tee subscriber connects");
    stream.set_nodelay(true).unwrap();
    let mut header = [0u8; 8];
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(n) = stream.peek(&mut header)
            && n >= 8
        {
            assert_eq!(&header, HEADER, "tee stream header");
            return stream;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("the tee never wrote its header");
}

/// Predicate that stops a collector once the records it needs have arrived.
type Done = Box<dyn Fn(&[Record]) -> bool + Send>;

/// Read records until `done` says stop, a `Closed` record arrives, or the socket times out.
pub fn collect(stream: TcpStream, timeout: Duration, done: Done) -> JoinHandle<Vec<Record>> {
    thread::spawn(move || {
        stream.set_read_timeout(Some(timeout)).unwrap();
        let reader = Reader::new(stream).expect("tee header");
        let mut records = Vec::new();
        for item in reader {
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

/// Like [`collect`], but only stops on `done` (or the socket timeout), so it keeps records from
/// several connections alive at once.
pub fn collect_multi(stream: TcpStream, timeout: Duration, done: Done) -> JoinHandle<Vec<Record>> {
    thread::spawn(move || {
        stream.set_read_timeout(Some(timeout)).unwrap();
        let reader = Reader::new(stream).expect("tee header");
        let mut records = Vec::new();
        for item in reader {
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

pub fn collected_bytes(records: &[Record], kind: Kind) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        if record.kind == kind {
            out.extend_from_slice(&record.data);
        }
    }
    out
}

pub fn dropped_bytes(records: &[Record]) -> u64 {
    records
        .iter()
        .filter_map(|record| match record.kind {
            Kind::Gap { dropped_bytes } => Some(dropped_bytes),
            _ => None,
        })
        .sum()
}

pub fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    condition()
}

/// Fake upstream that reads and discards; exposes the byte count it has received.
pub fn spawn_sink() -> (SocketAddr, Arc<AtomicU64>) {
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

/// Fake upstream that echoes every byte back.
pub fn spawn_echo() -> SocketAddr {
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

/// Fake upstream that records connections and all bytes received, for gate tests.
pub struct Probe {
    pub addr: SocketAddr,
    pub connections: Arc<AtomicUsize>,
    pub data: Arc<Mutex<Vec<u8>>>,
}

pub fn spawn_probe() -> Probe {
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

#[cfg(windows)]
pub fn process_threads() -> usize {
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
pub fn process_threads() -> usize {
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
pub fn process_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .count()
}

/// Open file descriptors; sockets count here like handles do on Windows.
#[cfg(unix)]
pub fn process_handles() -> usize {
    std::fs::read_dir("/dev/fd").expect("/dev/fd").count()
}

#[cfg(windows)]
pub fn process_handles() -> usize {
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

pub struct SlowReader {
    pub inner: TcpStream,
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

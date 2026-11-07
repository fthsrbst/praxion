use praxion_forwarder::{Config, Forwarder};
use praxion_tee::{Kind, Reader};

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod common;

use common::*;

// ---------------------------------------------------------------- tests

#[test]
fn independent_many_concurrent_connections_tee_reconstructs_per_connection() {
    const N: usize = 20;
    const PAYLOAD: usize = 64 * 1024;
    let upstream = spawn_echo();
    let (game, tee) = start_forwarder(test_config(
        upstream,
        64 * 1024 * 1024,
        Duration::from_secs(10),
    ));

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
        assert_eq!(
            sb.len(),
            handshake(777, 2).len() + PAYLOAD,
            "conn {} sb len",
            conn
        );
        assert_eq!(sb, cb, "conn {} directions differ", conn);
        assert!(
            records
                .iter()
                .any(|r| r.conn_id == conn && r.kind == Kind::Closed),
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
    client
        .write_all(&sent)
        .expect("client sends while tee is stalled");
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
        records.iter().any(|r| r.kind == Kind::Serverbound
            && r.data
                .windows(b"after-reset".len())
                .any(|w| w == b"after-reset")),
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
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
    let mut client = TcpStream::connect(game).unwrap();
    client.write_all(&[0x00]).unwrap();
    drop(client);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(probe.connections.load(Ordering::Relaxed), 0);
}

#[test]
fn independent_gate_ten_second_timeout() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
    let mut client = TcpStream::connect(game).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(12)))
        .unwrap();
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
fn independent_login_with_trailing_packet_forwards_extra_bytes_unchanged() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
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

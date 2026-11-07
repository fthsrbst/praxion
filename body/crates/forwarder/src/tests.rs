use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use praxion_tee::{Kind, Reader};

use super::*;

#[path = "../tests/common/mod.rs"]
mod common;

use common::*;

/// Fake upstream that accepts one connection, sends `server_bytes`, and collects what the client
/// sent until the client half-closes.
fn spawn_duplex(server_bytes: Vec<u8>) -> (SocketAddr, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("duplex accept");
        let _ = stream.set_nodelay(true);
        let mut reader = stream.try_clone().unwrap();
        let reader_handle = thread::spawn(move || {
            let mut collected = Vec::new();
            reader.read_to_end(&mut collected).expect("duplex read");
            collected
        });
        let mut writer = stream;
        writer.write_all(&server_bytes).expect("duplex write");
        writer.shutdown(Shutdown::Write).unwrap();
        let collected = reader_handle.join().unwrap();
        let _ = writer.shutdown(Shutdown::Both);
        collected
    });
    (addr, handle)
}

/// Read until the server closes this client; fail if it stays open.
fn expect_closed(mut stream: TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut buffer = [0u8; 64];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                panic!("the forwarder did not close a refused client");
            }
            Err(_) => return,
        }
    }
}

#[test]
fn forwarding_is_byte_exact_both_ways_and_the_tee_reconstructs_both_directions() {
    const LEN: usize = 50 * 1024 * 1024;
    let to_server = {
        let mut bytes = handshake(777, 2);
        bytes.extend(random_bytes(0xA11CE, LEN));
        bytes
    };
    let to_client = random_bytes(0xB0B, LEN);
    let (upstream, server) = spawn_duplex(to_client.clone());
    let (game, tee) = start_forwarder(test_config(
        upstream,
        256 * 1024 * 1024,
        Duration::from_secs(10),
    ));

    let subscriber = connect_tee(tee);
    let collector = collect(subscriber, Duration::from_secs(60), Box::new(|_| false));

    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();
    let expected_len = to_client.len();
    let client_reader = client.try_clone().unwrap();
    let reader = thread::spawn(move || {
        let mut received = vec![0u8; expected_len];
        let mut reader = client_reader;
        reader
            .read_exact(&mut received)
            .expect("client receives all bytes");
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).expect("client sees EOF");
        assert!(tail.is_empty(), "unexpected bytes after the payload");
        (received, to_client)
    });
    client
        .write_all(&to_server)
        .expect("client sends all bytes");
    client.shutdown(Shutdown::Write).unwrap();

    let (received, expected) = reader.join().unwrap();
    assert_eq!(received, expected, "client direction is byte exact");
    assert_eq!(
        server.join().unwrap(),
        to_server,
        "server direction is byte exact"
    );

    let records = collector.join().unwrap();
    assert_eq!(
        collected_bytes(&records, Kind::Serverbound),
        to_server,
        "tee reconstructs client-to-server bytes"
    );
    assert_eq!(
        collected_bytes(&records, Kind::Clientbound),
        expected,
        "tee reconstructs server-to-client bytes"
    );
    assert_eq!(
        dropped_bytes(&records),
        0,
        "a reading subscriber loses nothing"
    );
    assert!(
        records.iter().any(|r| r.kind == Kind::Opened),
        "Opened record"
    );
    assert!(
        records.iter().any(|r| r.kind == Kind::Closed),
        "Closed record"
    );
    assert!(records.iter().all(|r| r.conn_id == 0));
}

#[test]
fn a_subscriber_that_never_reads_never_slows_forwarding_and_gaps_add_up() {
    const PAYLOAD: usize = 5 * 1024 * 1024;
    let (upstream, received) = spawn_sink();
    let (game, tee) = start_forwarder(test_config(upstream, 128 * 1024, Duration::from_secs(10)));

    let stalled = connect_tee(tee);

    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();
    let mut sent = handshake(777, 2);
    sent.extend(random_bytes(0x5EED, PAYLOAD));

    let start = Instant::now();
    client
        .write_all(&sent)
        .expect("client sends while the tee is stalled");
    let write_time = start.elapsed();
    assert!(
        write_time < Duration::from_secs(5),
        "forwarding took {write_time:?} with a stalled subscriber"
    );
    assert!(
        wait_until(Duration::from_secs(10), || received.load(Ordering::Relaxed)
            as usize
            == sent.len()),
        "the server did not receive the payload"
    );

    // Start reading; then send a tail so the writer wakes and the final gap can be emitted.
    let collector = collect(stalled, Duration::from_secs(30), Box::new(|_| false));
    let tail = random_bytes(0x7A11, 64 * 1024);
    client.write_all(&tail).expect("tail write");
    sent.extend_from_slice(&tail);
    assert!(wait_until(Duration::from_secs(10), || {
        received.load(Ordering::Relaxed) as usize == sent.len()
    }));
    client.shutdown(Shutdown::Write).unwrap();

    let records = collector.join().unwrap();
    let teed = collected_bytes(&records, Kind::Serverbound).len() as u64;
    let gaps = dropped_bytes(&records);
    assert!(gaps > 0, "a stalled subscriber must produce gaps");
    assert_eq!(
        teed + gaps,
        sent.len() as u64,
        "bytes forwarded = bytes teed + bytes in gaps"
    );
    assert_eq!(
        collected_bytes(&records, Kind::Serverbound).len() as u64,
        teed
    );
    assert!(records.iter().any(|r| r.kind == Kind::Closed));
}

#[test]
fn a_subscriber_joining_mid_connection_gets_the_header_and_a_gap_for_the_open_connection() {
    let (upstream, received) = spawn_sink();
    let (game, tee) = start_forwarder(test_config(upstream, 1024 * 1024, Duration::from_secs(10)));

    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();
    let mut first = handshake(777, 2);
    first.extend_from_slice(b"opened-before-the-subscriber");
    client.write_all(&first).unwrap();
    assert!(wait_until(Duration::from_secs(5), || {
        received.load(Ordering::Relaxed) as usize == first.len()
    }));
    // Let the tee record the forwarded bytes (the copy thread calls it right after write_all).
    thread::sleep(Duration::from_millis(100));

    let subscriber = connect_tee(tee);
    let collector = collect(
        subscriber,
        Duration::from_secs(10),
        Box::new(|records| records.iter().any(|r| r.kind == Kind::Serverbound)),
    );
    let second = b"after-the-subscriber";
    client.write_all(second).unwrap();

    let records = collector.join().unwrap();
    assert_eq!(records[0].conn_id, 0);
    assert_eq!(
        records[0].kind,
        Kind::Gap {
            dropped_bytes: first.len() as u64
        },
        "the mid-join gap covers every byte forwarded before the subscriber arrived"
    );
    assert!(
        records.iter().any(|r| r.kind == Kind::Opened) == false,
        "Opened was before the subscriber and is replaced by the gap"
    );
    let after = records
        .iter()
        .rev()
        .find(|r| r.kind == Kind::Serverbound)
        .expect("a live serverbound record");
    assert_eq!(after.data.as_slice(), second.as_slice());

    client.shutdown(Shutdown::Write).unwrap();
}

#[test]
fn a_new_subscriber_replaces_the_old_one_without_touching_forwarding() {
    let (upstream, received) = spawn_sink();
    let (game, tee) = start_forwarder(test_config(upstream, 1024 * 1024, Duration::from_secs(10)));

    let old = connect_tee(tee);
    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();
    let mut first = handshake(777, 2);
    first.extend_from_slice(b"old-subscriber");
    client.write_all(&first).unwrap();
    assert!(wait_until(Duration::from_secs(5), || {
        received.load(Ordering::Relaxed) as usize == first.len()
    }));
    thread::sleep(Duration::from_millis(50));

    let new = connect_tee(tee);
    let collector = collect(new, Duration::from_secs(10), Box::new(|_| false));

    // The old socket must have been shut down.
    let mut old = old;
    old.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut sink = [0u8; 4096];
    loop {
        match old.read(&mut sink) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                panic!("the replaced subscriber was not disconnected");
            }
            Err(_) => break,
        }
    }
    drop(old);

    let second = b"new-subscriber-data";
    client.write_all(second).unwrap();
    client.shutdown(Shutdown::Write).unwrap();

    let records = collector.join().unwrap();
    assert!(records.iter().any(|r| matches!(r.kind, Kind::Gap { .. })));
    assert!(
        records
            .iter()
            .any(|r| r.kind == Kind::Serverbound && r.data.as_slice() == second.as_slice()),
        "the new subscriber gets live records"
    );
    assert!(wait_until(Duration::from_secs(5), || {
        received.load(Ordering::Relaxed) as usize == first.len() + second.len()
    }));
}

#[test]
fn a_disconnected_subscriber_does_not_affect_forwarding_or_the_next_subscriber() {
    let (upstream, received) = spawn_sink();
    let (game, tee) = start_forwarder(test_config(upstream, 1024 * 1024, Duration::from_secs(10)));

    let first = connect_tee(tee);
    drop(first);

    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();
    let mut head = handshake(777, 2);
    head.extend(random_bytes(0xD15C, 512 * 1024));
    client.write_all(&head).unwrap();
    assert!(wait_until(Duration::from_secs(5), || {
        received.load(Ordering::Relaxed) as usize == head.len()
    }));

    let second = connect_tee(tee);
    let collector = collect(second, Duration::from_secs(10), Box::new(|_| false));
    let tail = b"tail-after-resubscribe";
    client.write_all(tail).unwrap();
    client.shutdown(Shutdown::Write).unwrap();

    let records = collector.join().unwrap();
    assert!(records.iter().any(|r| matches!(r.kind, Kind::Gap { .. })));
    assert!(
        records
            .iter()
            .any(|r| r.kind == Kind::Serverbound && r.data.as_slice() == tail)
    );
    assert!(wait_until(Duration::from_secs(5), || {
        received.load(Ordering::Relaxed) as usize == head.len() + tail.len()
    }));
}

#[test]
fn gate_accepts_a_login_with_the_pinned_protocol() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
    let frame = handshake(777, 2);
    let mut client = TcpStream::connect(game).unwrap();
    client.write_all(&frame).unwrap();
    assert!(wait_until(Duration::from_secs(5), || {
        probe.connections.load(Ordering::Relaxed) == 1
    }));
    assert!(wait_until(Duration::from_secs(5), || {
        probe.data.lock().unwrap().as_slice() == frame.as_slice()
    }));
    assert!(!probe.data.lock().unwrap().is_empty());
}

#[test]
fn gate_refuses_another_protocol_login_without_contacting_upstream() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
    let frame = handshake(776, 2);
    let mut client = TcpStream::connect(game).unwrap();
    client.write_all(&frame).unwrap();
    expect_closed(client);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        probe.connections.load(Ordering::Relaxed),
        0,
        "a refused login must not open an upstream connection"
    );
}

#[test]
fn gate_lets_a_status_request_through_for_any_protocol() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
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
fn gate_refuses_a_legacy_ping() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
    let mut client = TcpStream::connect(game).unwrap();
    client.write_all(&[0xFE, 0x01, 0xFA]).unwrap();
    expect_closed(client);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(probe.connections.load(Ordering::Relaxed), 0);
}

#[test]
fn gate_accepts_a_first_frame_split_into_one_byte_writes() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
    let frame = handshake(777, 2);
    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();
    for byte in &frame {
        client.write_all(std::slice::from_ref(byte)).unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    assert!(wait_until(Duration::from_secs(5), || {
        probe.data.lock().unwrap().as_slice() == frame.as_slice()
    }));
    assert_eq!(probe.connections.load(Ordering::Relaxed), 1);
}

#[test]
fn gate_refuses_an_oversized_first_frame() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
    let mut client = TcpStream::connect(game).unwrap();
    let mut oversized = varint((MAX_FRAME + 1) as i32);
    oversized.extend_from_slice(&[0u8; 64]);
    client.write_all(&oversized).unwrap();
    expect_closed(client);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(probe.connections.load(Ordering::Relaxed), 0);
}

fn percentile(samples: &[i64], p: f64) -> i64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mut index = (p * sorted.len() as f64).ceil() as usize;
    if index == 0 {
        index = 1;
    }
    sorted[(index - 1).min(sorted.len() - 1)]
}

fn ping(stream: &mut TcpStream, seq: u32) -> Duration {
    let start = Instant::now();
    stream.write_all(&seq.to_be_bytes()).expect("ping write");
    let mut reply = [0u8; 4];
    stream.read_exact(&mut reply).expect("ping read");
    assert_eq!(u32::from_be_bytes(reply), seq);
    start.elapsed()
}

#[test]
fn added_latency_stays_under_five_milliseconds_at_p99() {
    let upstream = spawn_echo();
    let (game, _tee) = start_forwarder(test_config(upstream, 1024 * 1024, Duration::from_secs(10)));

    let mut direct = TcpStream::connect(upstream).unwrap();
    direct.set_nodelay(true).unwrap();
    let mut forwarded = TcpStream::connect(game).unwrap();
    forwarded.set_nodelay(true).unwrap();

    // The gate needs a real handshake first; the echo server returns it, so consume it.
    let frame = handshake(777, 1);
    forwarded.write_all(&frame).unwrap();
    let mut echoed = vec![0u8; frame.len()];
    forwarded.read_exact(&mut echoed).unwrap();
    assert_eq!(echoed, frame);

    let mut added = Vec::with_capacity(2000);
    for i in 0..2000u32 {
        // Gaps that are not multiples of 50 ms, so sampling cannot phase-lock to the game tick.
        thread::sleep(Duration::from_micros(400 + (i as u64 % 9) * 650));
        if i % 2 == 0 {
            let direct_time = ping(&mut direct, i);
            let forwarded_time = ping(&mut forwarded, i);
            added.push(forwarded_time.as_nanos() as i64 - direct_time.as_nanos() as i64);
        } else {
            let forwarded_time = ping(&mut forwarded, i);
            let direct_time = ping(&mut direct, i);
            added.push(forwarded_time.as_nanos() as i64 - direct_time.as_nanos() as i64);
        }
    }

    let p50 = percentile(&added, 0.50);
    let p99 = percentile(&added, 0.99);
    let p999 = percentile(&added, 0.999);
    println!(
        "added latency (n={}): p50={}us p99={}us p99.9={}us",
        added.len(),
        p50 / 1000,
        p99 / 1000,
        p999 / 1000
    );
    assert!(
        p99 < 5_000_000,
        "added p99 is {}us, over the 5 ms budget",
        p99 / 1000
    );
}

#[test]
fn stats_file_receives_events_off_the_forwarding_path() {
    let probe = spawn_probe();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!("forwarder-stats-{}.jsonl", std::process::id()));
    let mut config = test_config(probe.addr, 1024 * 1024, Duration::from_millis(150));
    config.stats = Some(path.clone());
    let (game, _tee) = start_forwarder(config);

    // One client that times out at the gate, one that passes it.
    let mut silent = TcpStream::connect(game).unwrap();
    silent.write_all(&[0x00]).unwrap();
    expect_closed(silent);
    let mut passing = TcpStream::connect(game).unwrap();
    passing.write_all(&handshake(777, 2)).unwrap();
    assert!(wait_until(Duration::from_secs(5), || {
        probe.connections.load(Ordering::Relaxed) == 1
    }));
    drop(passing);

    assert!(wait_until(Duration::from_secs(5), || {
        std::fs::read_to_string(&path)
            .map(|text| text.contains("\"event\":\"closed\""))
            .unwrap_or(false)
    }));
    let text = std::fs::read_to_string(&path).unwrap();
    for event in ["listen", "accept", "refused", "closed"] {
        assert!(
            text.contains(&format!("\"event\":\"{event}\"")),
            "stats file lacks a {event} line:\n{text}"
        );
    }
    for line in text.lines() {
        assert!(line.starts_with("{\"t\":"), "every line carries t: {line}");
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn gate_refuses_a_dripped_first_frame_at_one_absolute_deadline() {
    let probe = spawn_probe();
    let (game, _tee) = start_forwarder(test_config(
        probe.addr,
        1024 * 1024,
        Duration::from_secs(10),
    ));
    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();

    // Frame length 0x0F: the gate waits for 15 more bytes, and this client sends one byte every
    // two seconds, so the frame never completes. The bound must be one absolute deadline.
    let mut dripper = client.try_clone().unwrap();
    let writer = thread::spawn(move || {
        for _ in 0..6 {
            if dripper.write_all(&[0x0F]).is_err() {
                break;
            }
            thread::sleep(Duration::from_secs(2));
        }
    });

    client
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let start = Instant::now();
    let mut buffer = [0u8; 32];
    loop {
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let elapsed = start.elapsed();
    writer.join().unwrap();

    assert!(
        elapsed >= Duration::from_secs(9) && elapsed <= Duration::from_secs(13),
        "the gate accepted a dripped frame past its deadline, refusing after {elapsed:?}"
    );
    assert_eq!(
        probe.connections.load(Ordering::Relaxed),
        0,
        "a timed-out first frame must not contact upstream"
    );
}

#[test]
fn subscribe_keeps_the_old_subscriber_when_the_clone_fails() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let tee = Tee::start(listener, 1024 * 1024, Stats::disabled());

    let old = connect_tee(addr);
    old.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut reader = Reader::new(old).expect("tee header");
    tee.opened(0);
    let opened = reader.next().expect("Opened arrives").expect("record");
    assert_eq!(opened.kind, Kind::Opened);

    // The next subscriber cannot clone its socket. The old one must stay registered: the failed
    // subscriber must not advance the generation or clear the pending drops first.
    tee.fail_next_subscriber_clone();
    let mut rejected = TcpStream::connect(addr).unwrap();
    rejected
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buffer = [0u8; 8];
    loop {
        match rejected.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                panic!("the subscriber whose clone failed was left hanging");
            }
            Err(_) => break,
        }
    }

    tee.forwarded(
        0,
        praxion_tee::now_ns(),
        Kind::Serverbound,
        b"still-registered",
    );
    let record = reader
        .next()
        .expect("the old subscriber must still be connected")
        .expect("live record");
    assert_eq!(record.kind, Kind::Serverbound);
    assert_eq!(record.data, b"still-registered");
}

#[test]
fn a_writer_panic_unregisters_the_current_subscriber() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let tee = Tee::start(listener, 1024 * 1024, Stats::disabled());
    tee.opened(0);

    // The panic happens before the writer assigns `current` (the header of a new subscriber).
    // The writer must still unregister that generation, or later records queue against a dead
    // socket.
    tee.panic_next_writer_item();
    let mut old = TcpStream::connect(addr).unwrap();
    old.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buffer = [0u8; 16];
    loop {
        match old.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                panic!("a writer panic left the current subscriber registered");
            }
            Err(_) => break,
        }
    }
    drop(old);

    let new = connect_tee(addr);
    let collector = collect(
        new,
        Duration::from_secs(10),
        Box::new(|records| records.iter().any(|r| r.kind == Kind::Serverbound)),
    );
    tee.forwarded(
        0,
        praxion_tee::now_ns(),
        Kind::Serverbound,
        b"after-the-panic",
    );
    let records = collector.join().unwrap();
    assert!(
        records
            .iter()
            .any(|r| r.kind == Kind::Serverbound && r.data == b"after-the-panic"),
        "the next subscriber must receive live records: {records:?}"
    );
}

#[test]
fn a_refused_half_close_ends_the_connection_instead_of_waiting_for_the_peer() {
    const PAYLOAD: usize = 64 * 1024;
    // The echo upstream only closes its side after it sees the client's EOF, so a half-close that
    // never arrives would leave both the upstream and this connection open forever.
    let upstream = spawn_echo();
    let mut config = test_config(upstream, 1024 * 1024, Duration::from_secs(10));
    // Windows refuses the half-close with WSAENOTCONN for some duplicated handles; the forwarder
    // must not turn that into a connection that never ends.
    config.fail_half_close = true;
    let (game, tee) = start_forwarder(config);

    let subscriber = connect_tee(tee);
    let collector = collect(subscriber, Duration::from_secs(10), Box::new(|_| false));

    let mut client = TcpStream::connect(game).unwrap();
    client.set_nodelay(true).unwrap();
    let mut sent = handshake(777, 2);
    sent.extend(random_bytes(0xFA11, PAYLOAD));
    client.write_all(&sent).unwrap();
    client.shutdown(Shutdown::Write).unwrap();

    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                panic!("the forwarder held the connection open after a refused half-close")
            }
            Err(_) => break,
        }
    }

    let records = collector.join().unwrap();
    assert!(
        records
            .iter()
            .any(|r| r.conn_id == 0 && r.kind == Kind::Closed),
        "a refused half-close must still emit Closed: {records:?}"
    );
}

#[test]
fn handle_client_panic_closes_both_sockets_and_reports_it() {
    let probe = spawn_probe();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!("forwarder-panic-{}.jsonl", std::process::id()));
    let mut config = test_config(probe.addr, 1024 * 1024, Duration::from_secs(10));
    config.stats = Some(path.clone());
    config.panic_in_handle_client = true;
    let (game, _tee) = start_forwarder(config);

    let mut client = TcpStream::connect(game).unwrap();
    client.write_all(&handshake(777, 2)).unwrap();
    expect_closed(client);

    assert!(
        wait_until(Duration::from_secs(5), || {
            probe.connections.load(Ordering::Relaxed) == 1
        }),
        "the connection panicked before reaching upstream"
    );
    assert!(
        wait_until(Duration::from_secs(5), || {
            std::fs::read_to_string(&path)
                .map(|text| {
                    text.contains("\"event\":\"panic\"") && text.contains("\"event\":\"closed\"")
                })
                .unwrap_or(false)
        }),
        "the panic must emit a panic event and a Closed record"
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"event\":\"panic\""), "stats:\n{text}");
    let _ = std::fs::remove_file(path);
}

#[test]
fn config_refuses_a_zero_byte_tee_queue() {
    let error = Config::parse(["--tee-queue-bytes".to_string(), "0".to_string()])
        .expect_err("a zero-byte tee queue is refused");
    assert!(
        error.contains("greater than 0"),
        "the message must say what is wrong: {error}"
    );
}

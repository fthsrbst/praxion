use proxy::*;
use std::env;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[derive(Clone, Debug)]
struct Args {
    listen: String,
    upstream: String,
    capture: String,
    stats_every: u64,
}

fn parse_args() -> Args {
    let mut args = env::args().skip(1);
    let mut listen = String::new();
    let mut upstream = String::new();
    let mut capture = String::new();
    let mut stats_every: u64 = 10;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => {
                listen = args.next().unwrap_or_default();
            }
            "--upstream" => {
                upstream = args.next().unwrap_or_default();
            }
            "--capture" => {
                capture = args.next().unwrap_or_default();
            }
            "--stats-every" => {
                if let Some(s) = args.next() {
                    if let Ok(v) = s.parse::<u64>() {
                        stats_every = v;
                    }
                }
            }
            _ => {}
        }
    }
    Args {
        listen,
        upstream,
        capture,
        stats_every,
    }
}

fn main() {
    let args_vec: Vec<String> = env::args().collect();
    if args_vec.iter().any(|x| x == "--selftest") {
        run_selftest();
        return;
    }
    let args = parse_args();
    install_ctrl_handler();

    let listener = match TcpListener::bind(&args.listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind listen failed: {}", e);
            return;
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop_c = stop.clone();
    // watch CTRL_C -> stop
    // (serve checks CTRL_C_FLAG itself)
    serve(listener, args.upstream, args.capture, args.stats_every, stop_c);
}

// ---------- selftest ----------

fn run_selftest() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    // Echo server with trailer after client half-close
    let echo_listener = TcpListener::bind("127.0.0.1:0").expect("bind echo");
    let echo_addr = echo_listener.local_addr().unwrap().to_string();
    let echo_stop = Arc::new(AtomicBool::new(false));
    let echo_stop_t = echo_stop.clone();
    thread::spawn(move || {
        for stream in echo_listener.incoming() {
            if echo_stop_t.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(s) = stream {
                thread::spawn(move || {
                    let mut s = s;
                    let _ = s.set_nodelay(true);
                    let mut buf = [0u8; 4096];
                    let mut all = Vec::new();
                    loop {
                        match s.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                all.extend_from_slice(&buf[..n]);
                                let _ = s.write_all(&buf[..n]);
                            }
                            Err(_) => break,
                        }
                    }
                    // after client half-close, send trailer
                    let _ = s.write_all(b"TRAILER");
                    let _ = s.shutdown(std::net::Shutdown::Write);
                });
            }
        }
    });

    // Dead upstream port: bind then drop listener to ensure refused
    let dead_listener = TcpListener::bind("127.0.0.1:0").expect("bind dead");
    let dead_addr = dead_listener.local_addr().unwrap().to_string();
    drop(dead_listener);

    // Scenario 1: normal proxy
    let p1_listener = TcpListener::bind("127.0.0.1:0").expect("bind p1");
    let p1_addr = p1_listener.local_addr().unwrap();
    let cap1 = format!("selftest_{}.prxcap", now_ns());
    let stop1 = Arc::new(AtomicBool::new(false));
    let stop1t = stop1.clone();
    let echo_addr_c = echo_addr.clone();
    let cap1_c = cap1.clone();
    thread::spawn(move || {
        serve(p1_listener, echo_addr_c, cap1_c, 10, stop1t);
    });
    thread::sleep(Duration::from_millis(200));

    // Scenario 2: refused upstream proxy
    let p2_listener = TcpListener::bind("127.0.0.1:0").expect("bind p2");
    let p2_addr = p2_listener.local_addr().unwrap();
    let cap2 = format!("selftest_{}.prxcap", now_ns());
    let stop2 = Arc::new(AtomicBool::new(false));
    let stop2t = stop2.clone();
    let dead_addr_c = dead_addr.clone();
    let cap2_c = cap2.clone();
    thread::spawn(move || {
        serve(p2_listener, dead_addr_c, cap2_c, 10, stop2t);
    });
    thread::sleep(Duration::from_millis(200));

    // Run scenario 1 client
    let mut client = TcpStream::connect(p1_addr).expect("connect p1");
    client.set_nodelay(true).unwrap();
    // spawn reader before writing
    use std::sync::{Arc, Mutex};
    let received_buf = Arc::new(Mutex::new(Vec::new()));
    let rb = received_buf.clone();
    let client_reader = client.try_clone().expect("clone client");
    let reader_handle = thread::spawn(move || {
        let mut c = client_reader;
        let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 4096];
        loop {
            match c.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut v) = rb.lock() {
                        v.extend_from_slice(&buf[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    });

    // send chunks
    let mut sent_c2s: Vec<u8> = Vec::new();
    let mut write_errs: Vec<String> = Vec::new();
    for i in 0..100usize {
        let size = 100;
        let data: Vec<u8> = (0..size).map(|j| ((i + j) as u8).wrapping_mul(131)).collect();
        sent_c2s.extend_from_slice(&data);
        if let Err(e) = client.write_all(&data) {
            write_errs.push(format!("write err: {}", e));
        }
    }
    // half-close client write
    let _ = client.shutdown(std::net::Shutdown::Write);

    let _ = reader_handle.join();
    let received = received_buf.lock().unwrap().clone();
    // verify echoed bytes prefix
    let mut msgs: Vec<String> = Vec::new();
    msgs.extend(write_errs);
    if received.len() < sent_c2s.len() {
        msgs.push(format!(
            "scenario1: received {} < sent {}",
            received.len(),
            sent_c2s.len()
        ));
    } else {
        if &received[..sent_c2s.len()] != sent_c2s.as_slice() {
            msgs.push("scenario1: echoed bytes mismatch".to_string());
        }
        // trailer should be at end
        let trailer = b"TRAILER";
        if !received.ends_with(trailer) {
            msgs.push("scenario1: trailer missing (half-close delivery failed)".to_string());
        }
    }

    drop(client);
    thread::sleep(Duration::from_millis(1000));

    // Scenario 2 client: connect to refused upstream proxy
    let mut client2 = match TcpStream::connect(p2_addr) {
        Ok(c) => c,
        Err(e) => {
            msgs.push(format!("scenario2 connect failed: {}", e));
            // stop and finish
            stop1.store(true, Ordering::SeqCst);
            stop2.store(true, Ordering::SeqCst);
            echo_stop.store(true, Ordering::SeqCst);
            // print
            if msgs.is_empty() {
                println!("SELFTEST: PASS");
            } else {
                for m in &msgs {
                    println!("SELFTEST: FAIL: {}", m);
                }
            }
            return;
        }
    };
    client2.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut buf2 = [0u8; 256];
    loop {
        match client2.read(&mut buf2) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    drop(client2);
    thread::sleep(Duration::from_millis(1000));

    // Stop proxies
    stop1.store(true, Ordering::SeqCst);
    stop2.store(true, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(500));

    // Verify capture 1
    verify_capture(&cap1, &mut msgs, true, sent_c2s.len());
    // Verify capture 2 (refused upstream): must have open and close records
    verify_capture(&cap2, &mut msgs, false, 0);

    echo_stop.store(true, Ordering::SeqCst);

    if msgs.is_empty() {
        println!("SELFTEST: PASS");
    } else {
        for m in &msgs {
            println!("SELFTEST: FAIL: {}", m);
        }
        println!("SELFTEST: FAIL");
    }
}

fn verify_capture(path: &str, msgs: &mut Vec<String>, check_c2s: bool, expected_c2s_len: usize) {
    use std::fs::File;
    use std::io::Read;
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            msgs.push(format!("{}: open failed {}", path, e));
            return;
        }
    };
    let mut data = Vec::new();
    if f.read_to_end(&mut data).is_err() {
        msgs.push(format!("{}: read failed", path));
        return;
    }
    if !data.starts_with(HEADER) {
        msgs.push(format!("{}: bad header", path));
        return;
    }
    let mut pos = HEADER.len();
    let mut open_count = 0u32;
    let mut close_count = 0u32;
    let mut c2s_concat: Vec<u8> = Vec::new();
    let mut s2c_concat: Vec<u8> = Vec::new();
    while pos + 8 + 4 + 1 + 4 <= data.len() {
        if pos + 8 > data.len() {
            break;
        }
        let ts = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        let _ = ts;
        pos += 8;
        if pos + 4 > data.len() {
            break;
        }
        let _conn_id = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;
        if pos >= data.len() {
            break;
        }
        let dir = data[pos];
        pos += 1;
        if pos + 4 > data.len() {
            break;
        }
        let len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;
        if pos + len as usize > data.len() {
            msgs.push(format!("{}: truncated record", path));
            break;
        }
        let chunk = data[pos..pos + len as usize].to_vec();
        pos += len as usize;
        match dir {
            2 => open_count += 1,
            3 => close_count += 1,
            0 => c2s_concat.extend_from_slice(&chunk),
            1 => s2c_concat.extend_from_slice(&chunk),
            _ => {}
        }
    }
    if open_count == 0 {
        msgs.push(format!("{}: no open (dir=2) records", path));
    }
    if close_count == 0 {
        msgs.push(format!("{}: no close (dir=3) records", path));
    }
    if open_count != close_count {
        msgs.push(format!(
            "{}: open({}) != close({})",
            path, open_count, close_count
        ));
    }
    if check_c2s {
        if c2s_concat.len() != expected_c2s_len {
            msgs.push(format!(
                "{}: c2s concat len {} != {}",
                path,
                c2s_concat.len(),
                expected_c2s_len
            ));
        }
    }
    let _ = s2c_concat;
}

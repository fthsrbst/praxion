use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const HEADER: &[u8] = b"PRXCAP1\n";

#[derive(Clone, Debug)]
pub struct CaptureRecord {
    pub ts: u64,
    pub conn_id: u32,
    pub dir: u8,
    pub data: Vec<u8>,
}

pub fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

pub fn write_header(w: &mut BufWriter<File>) -> std::io::Result<()> {
    w.write_all(HEADER)
}

pub fn write_record(w: &mut BufWriter<File>, rec: &CaptureRecord) -> std::io::Result<()> {
    w.write_all(&rec.ts.to_le_bytes())?;
    w.write_all(&rec.conn_id.to_le_bytes())?;
    w.write_all(&[rec.dir])?;
    w.write_all(&(rec.data.len() as u32).to_le_bytes())?;
    w.write_all(&rec.data)?;
    Ok(())
}

// VarInt (strict)
pub fn encode_varint(value: i32) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut v = value as u32;
    for _ in 0..5 {
        let b = v & 0x7F;
        v >>= 7;
        if v == 0 {
            buf.push(b as u8);
            break;
        } else {
            buf.push((b | 0x80) as u8);
        }
    }
    buf
}

pub fn decode_varint(bytes: &[u8]) -> Option<(i32, usize)> {
    let mut result: u32 = 0;
    let mut shift = 0usize;
    for i in 0..5 {
        if i >= bytes.len() {
            return None;
        }
        let b = bytes[i];
        if i == 4 {
            // only lower 4 bits allowed in 5th byte
            if b & 0xF0 != 0 {
                // check continuation? For terminating byte, high bits beyond 4 must be zero
                // Actually standard: 5th byte contributes at most 4 bits.
                if b & 0x70 != 0 {
                    return None;
                }
            }
        }
        result |= ((b & 0x7F) as u32) << shift;
        if (b & 0x80) == 0 {
            return Some((result as i32, i + 1));
        }
        shift += 7;
        if i == 4 {
            return None; // continuation bit set on 5th byte -> malformed
        }
    }
    None
}

// nearest-rank index helper
pub fn nearest_rank_index(n: usize, p: f64) -> usize {
    if n == 0 {
        return 0;
    }
    let mut idx = (p * n as f64).ceil() as usize;
    if idx == 0 {
        idx = 1;
    }
    idx -= 1;
    if idx >= n {
        idx = n - 1;
    }
    idx
}

pub fn percentile_u64(samples: &[u64], p: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut v = samples.to_vec();
    v.sort_unstable();
    v[nearest_rank_index(v.len(), p)]
}

// Dir state
#[derive(Clone)]
pub struct DirState {
    pub write_cost: Arc<Mutex<Vec<u64>>>,
    pub sojourn: Arc<Mutex<Vec<u64>>>,
    pub bytes: Arc<AtomicU64>,
    pub sent: Arc<AtomicU64>,
    pub written: Arc<AtomicU64>,
    pub max_backlog: Arc<AtomicU64>,
}

impl DirState {
    pub fn new() -> Self {
        Self {
            write_cost: Arc::new(Mutex::new(Vec::new())),
            sojourn: Arc::new(Mutex::new(Vec::new())),
            bytes: Arc::new(AtomicU64::new(0)),
            sent: Arc::new(AtomicU64::new(0)),
            written: Arc::new(AtomicU64::new(0)),
            max_backlog: Arc::new(AtomicU64::new(0)),
        }
    }
}

pub struct ConnEntry {
    pub client: TcpStream,
    pub handle: thread::JoinHandle<()>,
}

// handle_connection
pub fn handle_connection(
    client_stream: TcpStream,
    upstream_addr: &str,
    conn_id: u32,
    tx: Sender<CaptureRecord>,
    c2s: DirState,
    s2c: DirState,
    capture_broken: Arc<AtomicBool>,
    records_dropped: Arc<AtomicU64>,
) {
    // write dir=2 right after accept, before connect
    let rec_open = CaptureRecord {
        ts: now_ns(),
        conn_id,
        dir: 2,
        data: vec![],
    };
    // producer side for open record: attribute to neither direction; just send
    let _ = tx.send(rec_open);

    // connect upstream
    let upstream_stream = match TcpStream::connect(upstream_addr) {
        Ok(s) => s,
        Err(_) => {
            // ensure dir=3
            let rec_close = CaptureRecord {
                ts: now_ns(),
                conn_id,
                dir: 3,
                data: vec![],
            };
            let _ = tx.send(rec_close);
            return;
        }
    };
    let _ = client_stream.set_nodelay(true);
    let _ = upstream_stream.set_nodelay(true);

    // try_clone
    let client_c2s = match client_stream.try_clone() {
        Ok(s) => s,
        Err(_) => {
            let rec_close = CaptureRecord {
                ts: now_ns(),
                conn_id,
                dir: 3,
                data: vec![],
            };
            let _ = tx.send(rec_close);
            return;
        }
    };
    let upstream_c2s = match upstream_stream.try_clone() {
        Ok(s) => s,
        Err(_) => {
            let rec_close = CaptureRecord {
                ts: now_ns(),
                conn_id,
                dir: 3,
                data: vec![],
            };
            let _ = tx.send(rec_close);
            return;
        }
    };
    let client_s2c = client_stream;
    let upstream_s2c = upstream_stream;

    let tx_c2s = tx.clone();
    let c2s_c = c2s.clone();
    let cb_c = capture_broken.clone();
    let rd_c = records_dropped.clone();
    let h1 = thread::spawn(move || {
        forward_dir(
            client_c2s,
            upstream_c2s,
            conn_id,
            0,
            tx_c2s,
            c2s_c,
            cb_c,
            rd_c,
        );
    });

    let tx_s2c = tx.clone();
    let s2c_c = s2c.clone();
    let cb_s = capture_broken.clone();
    let rd_s = records_dropped.clone();
    let h2 = thread::spawn(move || {
        forward_dir(
            upstream_s2c,
            client_s2c,
            conn_id,
            1,
            tx_s2c,
            s2c_c,
            cb_s,
            rd_s,
        );
    });

    let _ = h1.join();
    let _ = h2.join();

    // dir=3 exactly once
    let rec_close = CaptureRecord {
        ts: now_ns(),
        conn_id,
        dir: 3,
        data: vec![],
    };
    let _ = tx.send(rec_close);
}

fn forward_dir(
    mut from: TcpStream,
    mut to: TcpStream,
    conn_id: u32,
    dir: u8,
    tx: Sender<CaptureRecord>,
    state: DirState,
    capture_broken: Arc<AtomicBool>,
    records_dropped: Arc<AtomicU64>,
) {
    // thread-local sample buffers
    let mut local_wc: Vec<u64> = Vec::new();
    let mut local_so: Vec<u64> = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        let t_before_read = Instant::now();
        match from.read(&mut buf) {
            Ok(0) => {
                // EOF: shutdown write on `to` only
                let _ = to.shutdown(std::net::Shutdown::Write);
                break;
            }
            Ok(n) => {
                let t_read = Instant::now();
                // Capture timestamp = when read() returned (spikes/README.md contract), taken before
                // forwarding so a reply can never carry an earlier timestamp than its request.
                let ts_read = now_ns();
                // write_all
                if let Err(_) = to.write_all(&buf[0..n]) {
                    // write error: close both
                    let _ = from.shutdown(std::net::Shutdown::Both);
                    let _ = to.shutdown(std::net::Shutdown::Both);
                    break;
                }
                let t_done = Instant::now();
                let write_cost = t_done.duration_since(t_read).as_nanos() as u64;
                let sojourn = t_done.duration_since(t_before_read).as_nanos() as u64;
                local_wc.push(write_cost);
                local_so.push(sojourn);

                let bytes_counter = n as u64;
                state.bytes.fetch_add(bytes_counter, Ordering::Relaxed);

                // capture broken?
                if capture_broken.load(Ordering::Relaxed) {
                    records_dropped.fetch_add(1, Ordering::Relaxed);
                    // publish samples periodically
                    publish_if_needed(&mut local_wc, &state.write_cost);
                    publish_if_needed(&mut local_so, &state.sojourn);
                    continue;
                }

                // soft cap check
                let cur_sent = state.sent.load(Ordering::Relaxed);
                let written = state.written.load(Ordering::Relaxed);
                if cur_sent.saturating_sub(written) > 65536 {
                    records_dropped.fetch_add(1, Ordering::Relaxed);
                    publish_if_needed(&mut local_wc, &state.write_cost);
                    publish_if_needed(&mut local_so, &state.sojourn);
                    continue;
                }

                // producer-side backlog measure
                let s = state.sent.fetch_add(1, Ordering::Relaxed) + 1;
                let written2 = state.written.load(Ordering::Relaxed);
                let bl = s.saturating_sub(written2);
                // CAS max_backlog
                loop {
                    let cur = state.max_backlog.load(Ordering::Relaxed);
                    if bl <= cur {
                        break;
                    }
                    if state
                        .max_backlog
                        .compare_exchange_weak(cur, bl, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                    {
                        break;
                    }
                }

                let rec = CaptureRecord {
                    ts: ts_read,
                    conn_id,
                    dir,
                    data: buf[0..n].to_vec(),
                };
                match tx.send(rec) {
                    Ok(_) => {}
                    Err(_) => {
                        let _ = from.shutdown(std::net::Shutdown::Both);
                        let _ = to.shutdown(std::net::Shutdown::Both);
                        // publish remaining
                        publish_all(&mut local_wc, &state.write_cost);
                        publish_all(&mut local_so, &state.sojourn);
                        return;
                    }
                }

                // publish samples periodically
                if local_wc.len() >= 256 {
                    publish_all(&mut local_wc, &state.write_cost);
                }
                if local_so.len() >= 256 {
                    publish_all(&mut local_so, &state.sojourn);
                }
            }
            Err(_) => {
                let _ = from.shutdown(std::net::Shutdown::Both);
                let _ = to.shutdown(std::net::Shutdown::Both);
                break;
            }
        }
    }
    // publish remaining
    publish_all(&mut local_wc, &state.write_cost);
    publish_all(&mut local_so, &state.sojourn);
}

fn publish_if_needed(local: &mut Vec<u64>, shared: &Arc<Mutex<Vec<u64>>>) {
    if local.len() >= 256 {
        publish_all(local, shared);
    }
}

fn publish_all(local: &mut Vec<u64>, shared: &Arc<Mutex<Vec<u64>>>) {
    if local.is_empty() {
        return;
    }
    if let Ok(mut g) = shared.lock() {
        g.append(local);
        local.clear();
    }
}

// serve function
pub fn serve(
    listener: TcpListener,
    upstream: String,
    capture_path: String,
    stats_every: u64,
    stop: Arc<AtomicBool>,
) {
    let stats_every = if stats_every == 0 { 1 } else { stats_every };

    // open capture on main thread (fail loudly)
    let capture_file = match OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&capture_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("failed to open capture {}: {}", capture_path, e);
            return;
        }
    };
    let stats_path = format!("{}.stats.jsonl", capture_path);
    // truncate stats file
    let _ = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&stats_path);

    let (tx, rx): (Sender<CaptureRecord>, Receiver<CaptureRecord>) = mpsc::channel();

    let c2s = DirState::new();
    let s2c = DirState::new();
    let capture_broken = Arc::new(AtomicBool::new(false));
    let records_dropped = Arc::new(AtomicU64::new(0));
    let shutdown = stop.clone();

    // accumulators in writer thread
    let c2s_wc_w = c2s.write_cost.clone();
    let c2s_so_w = c2s.sojourn.clone();
    let s2c_wc_w = s2c.write_cost.clone();
    let s2c_so_w = s2c.sojourn.clone();
    let c2s_bytes_w = c2s.bytes.clone();
    let s2c_bytes_w = s2c.bytes.clone();
    let c2s_written_w = c2s.written.clone();
    let s2c_written_w = s2c.written.clone();
    let c2s_maxbl_w = c2s.max_backlog.clone();
    let s2c_maxbl_w = s2c.max_backlog.clone();
    let capture_broken_w = capture_broken.clone();
    let records_dropped_w = records_dropped.clone();

    let writer_handle = thread::spawn(move || {
        let mut writer = BufWriter::new(capture_file);
        let _ = write_header(&mut writer);
        let mut last_flush = Instant::now();
        let mut last_stats = Instant::now();

        let mut c2s_wc_all: Vec<u64> = Vec::new();
        let mut c2s_so_all: Vec<u64> = Vec::new();
        let mut s2c_wc_all: Vec<u64> = Vec::new();
        let mut s2c_so_all: Vec<u64> = Vec::new();

        loop {
            if CTRL_C_FLAG.load(Ordering::SeqCst) {
                shutdown.store(true, Ordering::SeqCst);
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(rec) => {
                    // collect samples periodically
                    // (mailboxes drained at stats time; here just write record)
                    if !capture_broken_w.load(Ordering::Relaxed) {
                        match write_record(&mut writer, &rec) {
                            Ok(()) => {
                                match rec.dir {
                                    0 => {
                                        c2s_written_w.fetch_add(1, Ordering::Relaxed);
                                    }
                                    1 => {
                                        s2c_written_w.fetch_add(1, Ordering::Relaxed);
                                    }
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                if !capture_broken_w.swap(true, Ordering::SeqCst) {
                                    eprintln!("capture write error: {}", e);
                                }
                            }
                        }
                    }
                    if last_flush.elapsed() >= Duration::from_secs(1) {
                        let _ = writer.flush();
                        last_flush = Instant::now();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if last_flush.elapsed() >= Duration::from_secs(1) {
                        let _ = writer.flush();
                        last_flush = Instant::now();
                    }
                    if shutdown.load(Ordering::SeqCst) {
                        // wait for disconnected
                    } else if last_stats.elapsed() >= Duration::from_secs(stats_every) {
                        // collect mailboxes
                        collect_mailbox(&c2s_wc_w, &mut c2s_wc_all);
                        collect_mailbox(&c2s_so_w, &mut c2s_so_all);
                        collect_mailbox(&s2c_wc_w, &mut s2c_wc_all);
                        collect_mailbox(&s2c_so_w, &mut s2c_so_all);
                        print_stats(
                            &c2s_wc_all,
                            &c2s_so_all,
                            &s2c_wc_all,
                            &s2c_so_all,
                            c2s_bytes_w.load(Ordering::Relaxed),
                            s2c_bytes_w.load(Ordering::Relaxed),
                            c2s_maxbl_w.load(Ordering::Relaxed),
                            s2c_maxbl_w.load(Ordering::Relaxed),
                            records_dropped_w.load(Ordering::Relaxed),
                            capture_broken_w.load(Ordering::Relaxed),
                            &stats_path,
                            false,
                        );
                        last_stats = Instant::now();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
        // flush
        let _ = writer.flush();
        // final collect
        collect_mailbox(&c2s_wc_w, &mut c2s_wc_all);
        collect_mailbox(&c2s_so_w, &mut c2s_so_all);
        collect_mailbox(&s2c_wc_w, &mut s2c_wc_all);
        collect_mailbox(&s2c_so_w, &mut s2c_so_all);
        print_stats(
            &c2s_wc_all,
            &c2s_so_all,
            &s2c_wc_all,
            &s2c_so_all,
            c2s_bytes_w.load(Ordering::Relaxed),
            s2c_bytes_w.load(Ordering::Relaxed),
            c2s_maxbl_w.load(Ordering::Relaxed),
            s2c_maxbl_w.load(Ordering::Relaxed),
            records_dropped_w.load(Ordering::Relaxed),
            capture_broken_w.load(Ordering::Relaxed),
            &stats_path,
            true,
        );
    });

    // listener non-blocking
    let _ = listener.set_nonblocking(true);
    let registry: Arc<Mutex<Vec<ConnEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let mut conn_id: u32 = 0;
    loop {
        if CTRL_C_FLAG.load(Ordering::SeqCst) {
            stop.store(true, Ordering::SeqCst);
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((client_stream, _addr)) => {
                // On Windows an accepted socket inherits the listener's non-blocking mode; the
                // forwarders need blocking reads (a WouldBlock read was treated as a dead connection).
                if client_stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let upstream_c = upstream.clone();
                let tx_c = tx.clone();
                let c2s_c = c2s.clone();
                let s2c_c = s2c.clone();
                let cb = capture_broken.clone();
                let rd = records_dropped.clone();
                let client_for_reg = match client_stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => {
                        // still try handle? skip
                        continue;
                    }
                };
                let cid = conn_id;
                conn_id = conn_id.wrapping_add(1);
                let handle = thread::spawn(move || {
                    handle_connection(client_stream, &upstream_c, cid, tx_c, c2s_c, s2c_c, cb, rd);
                });
                if let Ok(mut reg) = registry.lock() {
                    reg.push(ConnEntry {
                        client: client_for_reg,
                        handle,
                    });
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                } else {
                    break;
                }
            }
        }
    }

    // shutdown: shut down client sockets
    stop.store(true, Ordering::SeqCst);
    if let Ok(reg) = registry.lock() {
        for e in reg.iter() {
            let _ = e.client.shutdown(std::net::Shutdown::Both);
        }
    }
    // join all connection threads
    let handles: Vec<thread::JoinHandle<()>> = {
        if let Ok(mut reg) = registry.lock() {
            reg.drain(..).map(|e| e.handle).collect()
        } else {
            Vec::new()
        }
    };
    for h in handles {
        let _ = h.join();
    }
    // drop sender
    drop(tx);
    let _ = writer_handle.join();
}

fn collect_mailbox(src: &Arc<Mutex<Vec<u64>>>, dst: &mut Vec<u64>) {
    if let Ok(mut g) = src.lock() {
        let mut batch = std::mem::take(&mut *g);
        dst.append(&mut batch);
    }
}

fn print_stats(
    c2s_wc: &[u64],
    c2s_so: &[u64],
    s2c_wc: &[u64],
    s2c_so: &[u64],
    c2s_bytes: u64,
    s2c_bytes: u64,
    c2s_maxbl: u64,
    s2c_maxbl: u64,
    records_dropped: u64,
    capture_broken: bool,
    stats_path: &str,
    final_flag: bool,
) {
    // print p50/p99/p99.9/max for write_cost
    let c_w_p50 = percentile_u64(c2s_wc, 0.50);
    let c_w_p99 = percentile_u64(c2s_wc, 0.99);
    let c_w_p999 = percentile_u64(c2s_wc, 0.999);
    let c_w_max = if c2s_wc.is_empty() {
        0
    } else {
        *c2s_wc.iter().max().unwrap()
    };
    let c_s_p50 = percentile_u64(c2s_so, 0.50);
    let c_s_p99 = percentile_u64(c2s_so, 0.99);
    let c_s_p999 = percentile_u64(c2s_so, 0.999);
    let c_s_max = if c2s_so.is_empty() {
        0
    } else {
        *c2s_so.iter().max().unwrap()
    };

    let s_w_p50 = percentile_u64(s2c_wc, 0.50);
    let s_w_p99 = percentile_u64(s2c_wc, 0.99);
    let s_w_p999 = percentile_u64(s2c_wc, 0.999);
    let s_w_max = if s2c_wc.is_empty() {
        0
    } else {
        *s2c_wc.iter().max().unwrap()
    };
    let s_s_p50 = percentile_u64(s2c_so, 0.50);
    let s_s_p99 = percentile_u64(s2c_so, 0.99);
    let s_s_p999 = percentile_u64(s2c_so, 0.999);
    let s_s_max = if s2c_so.is_empty() {
        0
    } else {
        *s2c_so.iter().max().unwrap()
    };

    let fmt_p999 = |n: usize, v: u64| -> String {
        if n < 1000 {
            "-".to_string()
        } else {
            format!("{}", v / 1000)
        }
    };

    println!(
        "dir0(c2s): wc n={} p50={}us p99={}us p99.9={}us max={}us | sojourn n={} p50={}us p99={}us p99.9={}us max={}us | bytes={} max_backlog={} dropped_total={} broken={}",
        c2s_wc.len(),
        c_w_p50 / 1000,
        c_w_p99 / 1000,
        fmt_p999(c2s_wc.len(), c_w_p999),
        c_w_max / 1000,
        c2s_so.len(),
        c_s_p50 / 1000,
        c_s_p99 / 1000,
        fmt_p999(c2s_so.len(), c_s_p999),
        c_s_max / 1000,
        c2s_bytes,
        c2s_maxbl,
        records_dropped,
        capture_broken
    );
    // fix p99.9 print: fmt above divides; but for n>=1000 we divided already? Actually fmt_p999 receives v (ns) and divides. Good. But for p99.9 <1000 print "-". Good.
    println!(
        "dir1(s2c): wc n={} p50={}us p99={}us p99.9={}us max={}us | sojourn n={} p50={}us p99={}us p99.9={}us max={}us | bytes={} max_backlog={} dropped_total={} broken={}",
        s2c_wc.len(),
        s_w_p50 / 1000,
        s_w_p99 / 1000,
        fmt_p999(s2c_wc.len(), s_w_p999),
        s_w_max / 1000,
        s2c_so.len(),
        s_s_p50 / 1000,
        s_s_p99 / 1000,
        fmt_p999(s2c_so.len(), s_s_p999),
        s_s_max / 1000,
        s2c_bytes,
        s2c_maxbl,
        records_dropped,
        capture_broken
    );

    // JSON
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(stats_path)
    {
        let _ = writeln!(
            f,
            "{{\"t\":{},\"final\":{},\"dir0\":{{\"write_cost\":{{\"n\":{},\"p50\":{},\"p99\":{},\"p999\":{},\"max\":{}}},\"sojourn\":{{\"n\":{},\"p50\":{},\"p99\":{},\"p999\":{},\"max\":{}}},\"bytes\":{},\"max_backlog\":{}}},\"dir1\":{{\"write_cost\":{{\"n\":{},\"p50\":{},\"p99\":{},\"p999\":{},\"max\":{}}},\"sojourn\":{{\"n\":{},\"p50\":{},\"p99\":{},\"p999\":{},\"max\":{}}},\"bytes\":{},\"max_backlog\":{}}},\"records_dropped\":{},\"capture_broken\":{}}}",
            now_ns(),
            final_flag,
            c2s_wc.len(),
            c_w_p50 / 1000,
            c_w_p99 / 1000,
            c_w_p999 / 1000,
            c_w_max / 1000,
            c2s_so.len(),
            c_s_p50 / 1000,
            c_s_p99 / 1000,
            c_s_p999 / 1000,
            c_s_max / 1000,
            c2s_bytes,
            c2s_maxbl,
            s2c_wc.len(),
            s_w_p50 / 1000,
            s_w_p99 / 1000,
            s_w_p999 / 1000,
            s_w_max / 1000,
            s2c_so.len(),
            s_s_p50 / 1000,
            s_s_p99 / 1000,
            s_s_p999 / 1000,
            s_s_max / 1000,
            s2c_bytes,
            s2c_maxbl,
            records_dropped,
            capture_broken
        );
    }
}

// CTRL_C flag global
pub static CTRL_C_FLAG: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
pub fn install_ctrl_handler() {
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_handler), 1);
    }
}

// No graceful Ctrl-C on other hosts: the default signal action ends the process.
#[cfg(not(windows))]
pub fn install_ctrl_handler() {}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

#[cfg(windows)]
static CTRL_COUNT: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    const CTRL_C: u32 = 0;
    const CTRL_CLOSE: u32 = 2;
    const CTRL_LOGOFF: u32 = 5;
    const CTRL_SHUTDOWN: u32 = 6;
    match ctrl_type {
        CTRL_C => {
            let n = CTRL_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
            CTRL_C_FLAG.store(true, Ordering::SeqCst);
            if n >= 2 {
                0
            } else {
                1
            }
        }
        CTRL_CLOSE | CTRL_LOGOFF | CTRL_SHUTDOWN => 0,
        _ => 1,
    }
}

// selftest will be in binary

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_encode() {
        assert_eq!(encode_varint(0), vec![0x00]);
        assert_eq!(encode_varint(1), vec![0x01]);
        assert_eq!(encode_varint(127), vec![0x7f]);
        assert_eq!(encode_varint(128), vec![0x80, 0x01]);
        assert_eq!(encode_varint(255), vec![0xff, 0x01]);
        assert_eq!(encode_varint(25565), vec![0xdd, 0xc7, 0x01]);
        assert_eq!(encode_varint(2097151), vec![0xff, 0xff, 0x7f]);
        assert_eq!(encode_varint(2147483647), vec![0xff, 0xff, 0xff, 0xff, 0x07]);
        assert_eq!(encode_varint(-1), vec![0xff, 0xff, 0xff, 0xff, 0x0f]);
    }

    #[test]
    fn test_varint_decode() {
        assert_eq!(decode_varint(&[0x00]), Some((0, 1)));
        assert_eq!(decode_varint(&[0x01]), Some((1, 1)));
        assert_eq!(decode_varint(&[0x7f]), Some((127, 1)));
        assert_eq!(decode_varint(&[0x80, 0x01]), Some((128, 2)));
        assert_eq!(decode_varint(&[0xff, 0x01]), Some((255, 2)));
        assert_eq!(decode_varint(&[0xdd, 0xc7, 0x01]), Some((25565, 3)));
        assert_eq!(decode_varint(&[0xff, 0xff, 0x7f]), Some((2097151, 3)));
        assert_eq!(decode_varint(&[0xff, 0xff, 0xff, 0xff, 0x07]), Some((2147483647, 5)));
        assert_eq!(decode_varint(&[0xff, 0xff, 0xff, 0xff, 0x0f]), Some((-1, 5)));
    }

    #[test]
    fn test_varint_strict() {
        // malformed: continuation on 5th byte
        assert!(decode_varint(&[0xff, 0xff, 0xff, 0xff, 0xff]).is_none());
        // truncated
        assert!(decode_varint(&[]).is_none());
    }

    #[test]
    fn test_nearest_rank() {
        let v = vec![1u64, 2, 3, 4, 5];
        assert_eq!(percentile_u64(&v, 0.50), 3);
        // n=1
        assert_eq!(percentile_u64(&[42u64], 0.99), 42);
    }

    #[test]
    fn test_read_packet_strict() {
        // tested in pingbench binary? keep here minimal
    }
}

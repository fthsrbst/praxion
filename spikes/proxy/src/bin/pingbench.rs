use proxy::{decode_varint, encode_varint, nearest_rank_index};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Instant;

fn parse_args() -> (String, Option<String>, usize, i32) {
    let mut args = std::env::args().skip(1);
    let mut target = "127.0.0.1:25565".to_string();
    let mut interleave: Option<String> = None;
    let mut n: usize = 1000;
    let mut protocol: i32 = 777;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--target" => {
                if let Some(s) = args.next() {
                    target = s;
                }
            }
            "--interleave" => {
                if let Some(s) = args.next() {
                    interleave = Some(s);
                }
            }
            "--n" => {
                if let Some(s) = args.next() {
                    if let Ok(v) = s.parse::<usize>() {
                        n = v;
                    }
                }
            }
            "--protocol" => {
                if let Some(s) = args.next() {
                    if let Ok(v) = s.parse::<i32>() {
                        protocol = v;
                    }
                }
            }
            _ => {}
        }
    }
    (target, interleave, n, protocol)
}

fn split_host_port(target: &str) -> (String, u16) {
    // target like host:port
    if let Some((h, p)) = target.rsplit_once(':') {
        let port = p.parse::<u16>().unwrap_or(25565);
        (h.to_string(), port)
    } else {
        (target.to_string(), 25565)
    }
}

fn write_packet(stream: &mut TcpStream, packet_id: i32, data: &[u8]) -> std::io::Result<()> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&encode_varint(packet_id));
    payload.extend_from_slice(data);
    let mut packet = Vec::new();
    packet.extend_from_slice(&encode_varint(payload.len() as i32));
    packet.extend_from_slice(&payload);
    stream.write_all(&packet)?;
    stream.flush()?;
    Ok(())
}

fn read_packet(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    // read varint length strictly
    let mut len_bytes = Vec::new();
    loop {
        let mut b = [0u8; 1];
        stream.read_exact(&mut b)?;
        len_bytes.push(b[0]);
        if (b[0] & 0x80) == 0 {
            break;
        }
        if len_bytes.len() >= 5 {
            // malformed: continuation bit still set after 5 bytes
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "malformed varint length",
            ));
        }
    }
    let (plen, _) = match decode_varint(&len_bytes) {
        Some(v) => v,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bad varint",
            ))
        }
    };
    if plen <= 0 || plen > 2_097_152 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid packet length",
        ));
    }
    let mut payload = vec![0u8; plen as usize];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}

fn decode_varint_packet_id(payload: &[u8]) -> Option<(i32, &[u8])> {
    let (id, consumed) = decode_varint(payload)?;
    Some((id, &payload[consumed..]))
}

fn ping_once(
    target: &str,
    protocol: i32,
    iteration: usize,
) -> Result<i64, ()> {
    let mut stream = TcpStream::connect(target).map_err(|_| ())?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(1)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(1)));

    let (host, port) = split_host_port(target);

    // Handshake packet 0x00
    let mut handshake_data = Vec::new();
    handshake_data.extend_from_slice(&encode_varint(protocol));
    handshake_data.extend_from_slice(&encode_varint(host.len() as i32));
    handshake_data.extend_from_slice(host.as_bytes());
    handshake_data.extend_from_slice(&port.to_be_bytes());
    handshake_data.extend_from_slice(&encode_varint(1)); // next_state status
    if write_packet(&mut stream, 0x00, &handshake_data).is_err() {
        return Err(());
    }
    // Status Request
    if write_packet(&mut stream, 0x00, &[]).is_err() {
        return Err(());
    }
    // Read status response
    match read_packet(&mut stream) {
        Ok(resp) => {
            // verify packet id 0x00? status response id 0x00
            if let Some((id, _)) = decode_varint_packet_id(&resp) {
                if id != 0x00 {
                    return Err(());
                }
            } else {
                return Err(());
            }
        }
        Err(_) => return Err(()),
    }

    let payload: i64 = iteration as i64;
    let ping_data = payload.to_be_bytes();
    let start = Instant::now();
    if write_packet(&mut stream, 0x01, &ping_data).is_err() {
        return Err(());
    }
    match read_packet(&mut stream) {
        Ok(pong) => {
            let elapsed = start.elapsed().as_nanos() as i64;
            // verify: packet id 0x01 and payload equals
            if let Some((id, rest)) = decode_varint_packet_id(&pong) {
                if id != 0x01 {
                    return Err(());
                }
                if rest.len() < 8 {
                    return Err(());
                }
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&rest[..8]);
                if i64::from_be_bytes(arr) != payload {
                    return Err(());
                }
            } else {
                return Err(());
            }
            Ok(elapsed as i64)
        }
        Err(_) => Err(()),
    }
}

fn summarize(samples: &[i64]) -> (usize, i64, i64, i64, i64) {
    if samples.is_empty() {
        return (0, 0, 0, 0, 0);
    }
    let mut v: Vec<i64> = samples.to_vec();
    v.sort_unstable();
    let n = v.len();
    let p50 = v[nearest_rank_index(n, 0.50)];
    let p99 = v[nearest_rank_index(n, 0.99)];
    let p999 = v[nearest_rank_index(n, 0.999)];
    let maxv = v[n - 1];
    (n, p50, p99, p999, maxv)
}

fn print_dist(name: &str, samples: &[i64]) {
    let (n, p50, p99, p999, maxv) = summarize(samples);
    let fmt = |x: i64| x / 1000;
    let p999_str = if n < 1000 {
        "-".to_string()
    } else {
        format!("{}", fmt(p999))
    };
    println!(
        "{}: n={} p50={}us p99={}us p99.9={}us max={}us",
        name, n, fmt(p50), fmt(p99), p999_str, fmt(maxv)
    );
}

fn main() {
    let (target, interleave, n, protocol) = parse_args();

    if let Some(addr2) = interleave {
        // interleave mode
        let mut samples1: Vec<i64> = Vec::with_capacity(n);
        let mut samples2: Vec<i64> = Vec::with_capacity(n);
        let mut diffs: Vec<i64> = Vec::with_capacity(n);
        let mut failures = 0u32;
        for i in 0..n {
            // target
            match ping_once(&target, protocol, i) {
                Ok(rtt) => {
                    samples1.push(rtt);
                    // addr2
                    match ping_once(&addr2, protocol, i) {
                        Ok(rtt2) => {
                            samples2.push(rtt2);
                            diffs.push(rtt - rtt2);
                        }
                        Err(_) => {
                            failures += 1;
                        }
                    }
                }
                Err(_) => {
                    failures += 1;
                    // still try addr2? skip to keep pairs aligned
                }
            }
        }
        println!("target={}", target);
        print_dist("target", &samples1);
        println!("interleave={}", addr2);
        print_dist("interleave", &samples2);
        print_dist("diff(target-interleave)", &diffs);
        println!("failures={}", failures);
    } else {
        let mut samples: Vec<i64> = Vec::with_capacity(n);
        let mut failures = 0u32;
        for i in 0..n {
            match ping_once(&target, protocol, i) {
                Ok(rtt) => samples.push(rtt),
                Err(_) => failures += 1,
            }
        }
        println!("target={} protocol={} n={}", target, protocol, n);
        print_dist("rtt", &samples);
        println!("failures={}", failures);
    }
}


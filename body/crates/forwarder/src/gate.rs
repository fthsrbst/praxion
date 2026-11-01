//! The one and only parsing the forwarder does: the client's first frame.
//!
//! The handshake decides whether this client may talk to the pinned server. Everything after the
//! handshake is forwarded untouched. A status request (intent 1) is allowed for any protocol
//! number, because server-list pingers use their own; a login (2) or transfer (3) must name the
//! manifest's protocol (docs/architecture.md §4 "Version manifest").
//!
//! Limits come from the protocol: a handshake frame is far smaller than 1 KiB, so a larger frame
//! is treated as malformed instead of being buffered.

use std::io::{self, Read};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Largest handshake frame payload the gate accepts. The protocol caps the host at 255 bytes,
/// so this is already generous; anything bigger is not a handshake.
pub const MAX_FRAME: usize = 1024;

/// Why the gate closed a client without forwarding it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// First byte 0xFE: the pre-1.7 server-list ping, which is not a VarInt frame.
    Legacy,
    /// Not a well-formed handshake frame (bad VarInt, packet id, host, intent, trailing bytes).
    Malformed,
    /// The frame declares more than [`MAX_FRAME`] bytes, or more than that arrived unparsed.
    Oversized,
    /// The first frame did not arrive within the configured bound.
    Timeout,
    /// The client socket failed while the gate was reading.
    Io,
}

impl Refusal {
    /// Stable name for the stats log.
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Legacy => "legacy",
            Refusal::Malformed => "malformed",
            Refusal::Oversized => "oversized",
            Refusal::Timeout => "timeout",
            Refusal::Io => "io",
        }
    }
}

/// What the gate learned, plus every byte it read.
#[derive(Debug)]
pub struct FirstFrame {
    /// All bytes read from the client, including any bytes after the first frame (a client may
    /// pipeline its next packet). Forwarded unchanged, in order, after the gate passes.
    pub buffered: Vec<u8>,
    /// Protocol number named in the handshake.
    pub protocol: i32,
    /// 1 status, 2 login, 3 transfer.
    pub intent: i32,
}

/// Read the client's first frame and classify it. Leaves the client's read timeout unchanged;
/// the caller must reset it before forwarding.
///
/// The bound is one absolute deadline for the whole frame, not a per-read timeout: a client that
/// drips one byte just often enough would otherwise hold the gate open forever. Reads only until
/// the first frame is complete and refuses as soon as the frame's length prefix proves it is
/// oversized, so a hostile client cannot make the gate buffer indefinitely.
///
/// Takes `&TcpStream`: the socket is shared with the copy threads and is never duplicated, so the
/// gate borrows it (`Read` is implemented for `&TcpStream`).
pub fn read_first_frame(stream: &TcpStream, timeout: Duration) -> Result<FirstFrame, Refusal> {
    let deadline = Instant::now() + timeout;
    let mut buffered: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 256];
    let mut from = stream;
    loop {
        match inspect(&buffered) {
            Ok(Some((protocol, intent))) => {
                return Ok(FirstFrame {
                    buffered,
                    protocol,
                    intent,
                });
            }
            Ok(None) => {}
            Err(refusal) => return Err(refusal),
        }
        if buffered.len() > MAX_FRAME + 5 {
            return Err(Refusal::Oversized);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(Refusal::Timeout);
        }
        // Recompute the remaining budget every iteration. Round up to 1 ms: a zero timeout is
        // rejected by the OS and would turn into an unbounded wait.
        let remaining = (deadline - now).max(Duration::from_millis(1));
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| Refusal::Io)?;
        match from.read(&mut chunk) {
            Ok(0) => return Err(Refusal::Malformed),
            Ok(n) => buffered.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(Refusal::Timeout);
            }
            Err(_) => return Err(Refusal::Io),
        }
    }
}

/// Parse a complete handshake from `buf`. `Ok(None)` means "not enough bytes yet".
fn inspect(buf: &[u8]) -> Result<Option<(i32, i32)>, Refusal> {
    let Some(&first) = buf.first() else {
        return Ok(None);
    };
    if first == 0xFE {
        return Err(Refusal::Legacy);
    }
    let Some((frame_len, prefix)) = varint_at(buf, 0)? else {
        return Ok(None);
    };
    if frame_len < 0 {
        return Err(Refusal::Malformed);
    }
    let frame_len = frame_len as usize;
    if frame_len == 0 {
        return Err(Refusal::Malformed);
    }
    if frame_len > MAX_FRAME {
        return Err(Refusal::Oversized);
    }
    if buf.len() < prefix + frame_len {
        return Ok(None);
    }
    let payload = &buf[prefix..prefix + frame_len];
    let mut at = 0usize;

    let Some((packet_id, n)) = varint_at(payload, at)? else {
        return Err(Refusal::Malformed);
    };
    at += n;
    if packet_id != 0 {
        return Err(Refusal::Malformed);
    }

    let Some((protocol, n)) = varint_at(payload, at)? else {
        return Err(Refusal::Malformed);
    };
    at += n;
    if protocol < 0 {
        return Err(Refusal::Malformed);
    }

    let Some((host_len, n)) = varint_at(payload, at)? else {
        return Err(Refusal::Malformed);
    };
    at += n;
    if !(0..=255).contains(&host_len) {
        return Err(Refusal::Malformed);
    }
    let host_len = host_len as usize;
    if payload.len() < at + host_len + 2 {
        return Err(Refusal::Malformed);
    }
    if std::str::from_utf8(&payload[at..at + host_len]).is_err() {
        return Err(Refusal::Malformed);
    }
    at += host_len;
    // u16 port, big endian; the value itself does not matter to the gate.
    at += 2;

    let Some((intent, n)) = varint_at(payload, at)? else {
        return Err(Refusal::Malformed);
    };
    at += n;
    if !(1..=3).contains(&intent) {
        return Err(Refusal::Malformed);
    }
    if at != payload.len() {
        return Err(Refusal::Malformed);
    }
    Ok(Some((protocol, intent)))
}

/// Strict protocol VarInt: at most 5 bytes, the fifth one only 4 payload bits and no continuation.
/// `Ok(None)` means the bytes end before the VarInt does.
fn varint_at(buf: &[u8], offset: usize) -> Result<Option<(i32, usize)>, Refusal> {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    let mut at = offset;
    for _ in 0..5 {
        let Some(&byte) = buf.get(at) else {
            return Ok(None);
        };
        at += 1;
        if shift == 28 && (byte & 0xF0) != 0 {
            return Err(Refusal::Malformed);
        }
        result |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(Some((result as i32, at - offset)));
        }
        shift += 7;
    }
    Err(Refusal::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn accepts_login_and_status() {
        assert_eq!(inspect(&handshake(777, 2)), Ok(Some((777, 2))));
        assert_eq!(inspect(&handshake(776, 1)), Ok(Some((776, 1))));
        assert_eq!(inspect(&handshake(777, 3)), Ok(Some((777, 3))));
    }

    #[test]
    fn waits_for_more_without_the_whole_frame() {
        let frame = handshake(777, 2);
        for cut in 0..frame.len() {
            assert!(matches!(inspect(&frame[..cut]), Ok(None)), "cut {cut}");
        }
    }

    #[test]
    fn refuses_legacy_and_oversized() {
        assert_eq!(inspect(&[0xFE]), Err(Refusal::Legacy));
        assert_eq!(inspect(&[0xFE, 0x01, 0xFA]), Err(Refusal::Legacy));
        let oversized = {
            let mut frame = varint((MAX_FRAME + 1) as i32);
            frame.extend(std::iter::repeat_n(0u8, MAX_FRAME + 1));
            frame
        };
        assert_eq!(inspect(&oversized), Err(Refusal::Oversized));
    }

    #[test]
    fn refuses_malformed() {
        // packet id 1 is not a handshake
        let mut frame = handshake(777, 2);
        frame[1] = 1;
        assert_eq!(inspect(&frame), Err(Refusal::Malformed));
        // intent 4
        let mut f = handshake(777, 2);
        *f.last_mut().unwrap() = 4;
        assert_eq!(inspect(&f), Err(Refusal::Malformed));
        // trailing byte inside the frame
        let mut f = handshake(777, 2);
        f.push(0);
        let len = f.len() - 1;
        f[0] = len as u8;
        assert_eq!(inspect(&f), Err(Refusal::Malformed));
        // a VarInt must not continue past five bytes
        assert_eq!(inspect(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]), Err(Refusal::Malformed));
    }
}

//! Asynchronous stats log: one JSON object per line, written by its own thread.
//!
//! Callers hand events to an unbounded channel; the send never touches a file, so the forwarding
//! and tee paths cannot be slowed down by a slow disk. A summary line follows every 10 s.
//! If this thread panics, the next loop iteration catches it, logs, and keeps going; callers keep
//! sending. If the file cannot be opened at startup the forwarder runs without stats (a stats
//! failure is never a reason to drop a game connection).

use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use praxion_tee::now_ns;

/// One logged event.
#[derive(Debug)]
pub enum Event {
    /// The forwarder bound its listeners and is ready.
    Listen {
        listen: String,
        upstream: String,
        tee_listen: String,
        tee_queue_bytes: usize,
        protocol: u32,
    },
    /// A client TCP connection was accepted.
    Accept { conn: u32, peer: String },
    /// The protocol gate closed a client before connecting upstream.
    Refused {
        conn: u32,
        peer: String,
        reason: &'static str,
    },
    /// The per-connection handler panicked; both sockets were closed and the connection finished.
    Panic { conn: u32, peer: String },
    /// A client connection ended; bytes are what the forwarder actually forwarded.
    Closed {
        conn: u32,
        serverbound: u64,
        clientbound: u64,
    },
    /// A tee subscriber connected (replacing the previous one if any).
    TeeSubscribed { peer: String, replaced: bool },
    /// The tee dropped this many forwarded bytes for one connection.
    TeeDropped { conn: u32, bytes: u64 },
    /// The tee told the subscriber about lost bytes for one connection.
    Gap { conn: u32, dropped_bytes: u64 },
}

/// Cloneable stats sink. Cheap to clone; a no-op when stats are disabled.
#[derive(Clone)]
pub struct Stats {
    tx: Option<Sender<Event>>,
}

impl Stats {
    /// A sink that discards everything.
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    /// Open `path` (truncating it) and start the writer thread.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("stats".into())
            .spawn(move || run(file, rx))?;
        Ok(Self { tx: Some(tx) })
    }

    /// Queue one event. Never blocks and never touches the file.
    pub fn send(&self, event: Event) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(event);
        }
    }
}

#[derive(Default)]
struct Counters {
    open: u64,
    accepts: u64,
    refused: u64,
    closed: u64,
    serverbound: u64,
    clientbound: u64,
    tee_dropped_bytes: u64,
    gaps: u64,
    tee_subscribers: u64,
}

fn run(file: std::fs::File, rx: Receiver<Event>) {
    // A fresh file per run; logs after this one are ignored rather than failing the process.
    let mut out = BufWriter::new(file);
    let mut counters = Counters::default();
    let mut last_summary = Instant::now();
    loop {
        let received = rx.recv_timeout(Duration::from_millis(250));
        let event = match received {
            Ok(event) => Some(event),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(event) = event {
                counters.apply(&event);
                if let Some(line) = counters.line(event) {
                    write_line(&mut out, &line);
                }
            }
            if last_summary.elapsed() >= Duration::from_secs(10) {
                let line = counters.summary();
                write_line(&mut out, &line);
                last_summary = Instant::now();
            }
        }));
        if step.is_err() {
            log::warn!("forwarder: stats thread panicked while writing an event; continuing");
        }
    }
    let _ = out.flush();
}

fn write_line<W: Write>(out: &mut W, line: &str) {
    if writeln!(out, "{line}").is_err() || out.flush().is_err() {
        // Keep reading events so the channel does not grow; the disk is the broken part.
        log::warn!("forwarder: stats write failed; events are counted but not written");
    }
}

impl Counters {
    fn apply(&mut self, event: &Event) {
        match event {
            Event::Accept { .. } => {
                self.accepts += 1;
                self.open += 1;
            }
            Event::Refused { .. } => {
                self.refused += 1;
                self.open = self.open.saturating_sub(1);
            }
            Event::Panic { .. } => {}
            Event::Closed {
                serverbound,
                clientbound,
                ..
            } => {
                self.closed += 1;
                self.open = self.open.saturating_sub(1);
                self.serverbound += serverbound;
                self.clientbound += clientbound;
            }
            Event::TeeSubscribed { .. } => self.tee_subscribers += 1,
            Event::TeeDropped { bytes, .. } => self.tee_dropped_bytes += bytes,
            Event::Gap { .. } => self.gaps += 1,
            Event::Listen { .. } => {}
        }
    }

    fn line(&self, event: Event) -> Option<String> {
        let t = now_ns();
        Some(match event {
            Event::Listen {
                listen,
                upstream,
                tee_listen,
                tee_queue_bytes,
                protocol,
            } => format!(
                "{{\"t\":{t},\"event\":\"listen\",\"listen\":\"{}\",\"upstream\":\"{}\",\"tee_listen\":\"{}\",\"tee_queue_bytes\":{tee_queue_bytes},\"protocol\":{protocol}}}",
                esc(&listen),
                esc(&upstream),
                esc(&tee_listen)
            ),
            Event::Accept { conn, peer } => format!(
                "{{\"t\":{t},\"event\":\"accept\",\"conn\":{conn},\"peer\":\"{}\"}}",
                esc(&peer)
            ),
            Event::Refused { conn, peer, reason } => format!(
                "{{\"t\":{t},\"event\":\"refused\",\"conn\":{conn},\"peer\":\"{}\",\"reason\":\"{reason}\"}}",
                esc(&peer)
            ),
            Event::Panic { conn, peer } => format!(
                "{{\"t\":{t},\"event\":\"panic\",\"conn\":{conn},\"peer\":\"{}\"}}",
                esc(&peer)
            ),
            Event::Closed {
                conn,
                serverbound,
                clientbound,
            } => format!(
                "{{\"t\":{t},\"event\":\"closed\",\"conn\":{conn},\"serverbound\":{serverbound},\"clientbound\":{clientbound}}}"
            ),
            Event::TeeSubscribed { peer, replaced } => format!(
                "{{\"t\":{t},\"event\":\"tee_subscribed\",\"peer\":\"{}\",\"replaced\":{replaced}}}",
                esc(&peer)
            ),
            Event::TeeDropped { conn, bytes } => format!(
                "{{\"t\":{t},\"event\":\"tee_dropped\",\"conn\":{conn},\"bytes\":{bytes}}}"
            ),
            Event::Gap {
                conn,
                dropped_bytes,
            } => format!(
                "{{\"t\":{t},\"event\":\"gap\",\"conn\":{conn},\"dropped_bytes\":{dropped_bytes}}}"
            ),
        })
    }

    fn summary(&self) -> String {
        format!(
            "{{\"t\":{},\"event\":\"summary\",\"open\":{},\"accepts\":{},\"refused\":{},\"closed\":{},\"serverbound\":{},\"clientbound\":{},\"tee_dropped_bytes\":{},\"gaps\":{},\"tee_subscribers\":{}}}",
            now_ns(),
            self.open,
            self.accepts,
            self.refused,
            self.closed,
            self.serverbound,
            self.clientbound,
            self.tee_dropped_bytes,
            self.gaps,
            self.tee_subscribers
        )
    }
}

/// Escape a string for a JSON value. Our strings are addresses and reasons, but an error path
/// could still contain a quote.
fn esc(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_json_strings() {
        assert_eq!(esc("a\"b\\c\n"), "a\\\"b\\\\c\\n");
        assert_eq!(esc("\u{1}"), "\\u0001");
    }
}

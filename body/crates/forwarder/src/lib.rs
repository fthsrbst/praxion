//! The forwarder: the minimal process between the Minecraft client and the vanilla server.
//!
//! It forwards bytes untouched. Its only parsing is the client's first handshake frame, so a
//! client that would speak another protocol number never reaches the server
//! (docs/architecture.md §4, "Version manifest"). Every forwarded chunk is also handed to a tee
//! for the body; that path is bounded and never waits, so a dead or slow body cannot drop, slow
//! or block the game connection (docs/decisions.md, "Hard requirement: no body or decoder crash
//! may drop the user's game connection").

mod gate;
mod stats;
mod tee;

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use praxion_manifest::Manifest;
use praxion_tee::{now_ns, Kind};

pub use gate::MAX_FRAME;
use stats::{Event, Stats};
use tee::Tee;

/// Protocol assumed when `versions/manifest.toml` cannot be read. The manifest is the source;
/// this only keeps the forwarder running (and exiting solely on a listen error).
pub const FALLBACK_PROTOCOL: u32 = 777;

/// Default byte budget of the tee queue (64 MiB).
pub const DEFAULT_TEE_QUEUE_BYTES: usize = 64 * 1024 * 1024;
/// Default bound on waiting for the client's first frame.
pub const DEFAULT_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
/// The socket buffer of both copy threads.
const COPY_BUFFER_BYTES: usize = 64 * 1024;
/// Bound on reaching the upstream server. Loopback connects are immediate; a black-holed address
/// must not hold a connection thread, or its client socket, open indefinitely.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Everything the forwarder needs to run.
#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub upstream: SocketAddr,
    pub tee_listen: SocketAddr,
    pub stats: Option<PathBuf>,
    pub tee_queue_bytes: usize,
    pub protocol: u32,
    /// Bound on the wait for the client's first frame. Not a CLI flag; tests shorten it.
    pub first_frame_timeout: Duration,
    /// Test-only fault injection: panic after the upstream connection is established.
    #[cfg(test)]
    pub panic_in_handle_client: bool,
    /// Test-only fault injection: behave as if the platform refused the half-close (`shutdown`),
    /// which Windows really does for some duplicated handles (WSAENOTCONN).
    #[cfg(test)]
    pub fail_half_close: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 25565)),
            upstream: SocketAddr::from(([127, 0, 0, 1], 25566)),
            tee_listen: SocketAddr::from(([127, 0, 0, 1], 25600)),
            stats: None,
            tee_queue_bytes: DEFAULT_TEE_QUEUE_BYTES,
            protocol: default_protocol(),
            first_frame_timeout: DEFAULT_FIRST_FRAME_TIMEOUT,
            #[cfg(test)]
            panic_in_handle_client: false,
            #[cfg(test)]
            fail_half_close: false,
        }
    }
}

impl Config {
    /// Parse the command line. `--listen`, `--upstream` and `--tee-listen` take `IP:port`;
    /// `--stats` takes a path; `--tee-queue-bytes` a decimal byte count.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut config = Self::default();
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match flag.as_str() {
                "--listen" => config.listen = parse_addr(&value()?, "--listen")?,
                "--upstream" => config.upstream = parse_addr(&value()?, "--upstream")?,
                "--tee-listen" => config.tee_listen = parse_addr(&value()?, "--tee-listen")?,
                "--stats" => config.stats = Some(PathBuf::from(value()?)),
                "--tee-queue-bytes" => {
                    config.tee_queue_bytes = value()?
                        .parse()
                        .map_err(|_| "--tee-queue-bytes must be a byte count".to_string())?;
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        if config.tee_queue_bytes == 0 {
            return Err(
                "--tee-queue-bytes must be greater than 0; a zero-byte tee queue would silently \
                 disable the tee"
                    .into(),
            );
        }
        if !config.listen.ip().is_loopback() {
            return Err("--listen must be a loopback address; the game connection is local".into());
        }
        if !config.tee_listen.ip().is_loopback() {
            return Err("--tee-listen must be a loopback address; the tee is local".into());
        }
        Ok(config)
    }
}

fn parse_addr(value: &str, flag: &str) -> Result<SocketAddr, String> {
    SocketAddr::from_str(value).map_err(|error| format!("{flag}: {value}: {error}"))
}

fn default_protocol() -> u32 {
    match Manifest::load_default() {
        Ok(manifest) => manifest.minecraft.protocol,
        Err(error) => {
            log::warn!(
                "forwarder: cannot read versions/manifest.toml ({error}); assuming protocol {FALLBACK_PROTOCOL}"
            );
            FALLBACK_PROTOCOL
        }
    }
}

/// A bound forwarder. Binding both listeners is the only failure that ends the process.
pub struct Forwarder {
    config: Config,
    listener: TcpListener,
    tee_listener: TcpListener,
}

impl Forwarder {
    /// Bind the game listener and the tee listener. A failure here is a listen error.
    pub fn bind(config: Config) -> io::Result<Self> {
        let listener = TcpListener::bind(config.listen)?;
        let tee_listener = TcpListener::bind(config.tee_listen)?;
        Ok(Self {
            config,
            listener,
            tee_listener,
        })
    }

    /// Address the game client connects to. Useful when configured with port 0.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Address the body connects to for the tee stream.
    pub fn tee_local_addr(&self) -> io::Result<SocketAddr> {
        self.tee_listener.local_addr()
    }

    /// Run until the process is stopped. Accept errors are logged and retried; they do not exit.
    pub fn run(self) {
        let stats = match &self.config.stats {
            Some(path) => match Stats::open(path) {
                Ok(stats) => stats,
                Err(error) => {
                    log::warn!(
                        "forwarder: cannot open stats file {} ({error}); running without stats",
                        path.display()
                    );
                    Stats::disabled()
                }
            },
            None => Stats::disabled(),
        };
        stats.send(Event::Listen {
            listen: self.listener.local_addr().map(|a| a.to_string()).unwrap_or_default(),
            upstream: self.config.upstream.to_string(),
            tee_listen: self.tee_listener.local_addr().map(|a| a.to_string()).unwrap_or_default(),
            tee_queue_bytes: self.config.tee_queue_bytes,
            protocol: self.config.protocol,
        });
        eprintln!(
            "forwarder: listening on {} -> {}, tee on {}",
            self.listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_default(),
            self.config.upstream,
            self.tee_listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_default()
        );

        let tee = Tee::start(
            self.tee_listener,
            self.config.tee_queue_bytes,
            stats.clone(),
        );
        let mut next_conn: u32 = 0;
        for incoming in self.listener.incoming() {
            let client = match incoming {
                Ok(client) => client,
                Err(error) => {
                    log::warn!("forwarder: accept failed: {error}");
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            let conn = next_conn;
            next_conn = next_conn.wrapping_add(1);
            let peer = client
                .peer_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| "unknown".into());
            stats.send(Event::Accept {
                conn,
                peer: peer.clone(),
            });
            tee.opened(conn);
            let config = self.config.clone();
            let thread_tee = tee.clone();
            let thread_stats = stats.clone();
            let spawned = thread::Builder::new()
                .name(format!("conn-{conn}"))
                .spawn(move || handle_client(client, conn, peer, config, thread_tee, thread_stats));
            if let Err(error) = spawned {
                log::warn!("forwarder: cannot start conn {conn}: {error}");
                tee.closed(conn);
                stats.send(Event::Closed {
                    conn,
                    serverbound: 0,
                    clientbound: 0,
                });
            }
        }
    }
}

/// Run one connection, closing both sockets and finishing the connection if it panics.
///
/// A panic on the forwarding thread must behave like any other connection end: the client and the
/// upstream socket are shut down, the tee gets `Closed`, and a `panic` stats event records it. The
/// process and every other connection continue.
///
/// Both sockets are shared with the two copy threads through `Arc`, never duplicated: a Windows
/// handle duplicated while `connect` is still being registered can keep reporting "not connected"
/// (WSAENOTCONN) for `shutdown` even though reads and writes on the same socket work. A half-close
/// sent through such a handle never reaches the peer, which is how a connection ends up hanging.
fn handle_client(
    client: TcpStream,
    conn: u32,
    peer: String,
    config: Config,
    tee: Tee,
    stats: Stats,
) {
    let client = Arc::new(client);
    let mut upstream: Option<Arc<TcpStream>> = None;
    let result = catch_unwind(AssertUnwindSafe(|| {
        handle_client_inner(
            &client,
            conn,
            &peer,
            &config,
            &tee,
            &stats,
            &mut upstream,
        );
    }));
    if result.is_err() {
        log::warn!("forwarder: conn {conn} {peer} panicked; closing both directions");
        let _ = client.shutdown(Shutdown::Both);
        if let Some(upstream) = &upstream {
            let _ = upstream.shutdown(Shutdown::Both);
        }
        stats.send(Event::Panic {
            conn,
            peer: peer.clone(),
        });
        finish(conn, 0, 0, &tee, &stats);
    }
}

fn handle_client_inner(
    client: &Arc<TcpStream>,
    conn: u32,
    peer: &str,
    config: &Config,
    tee: &Tee,
    stats: &Stats,
    upstream_guard: &mut Option<Arc<TcpStream>>,
) {
    let _ = client.set_nonblocking(false);
    let _ = client.set_nodelay(true);

    let first = match gate::read_first_frame(client, config.first_frame_timeout) {
        Ok(first) => first,
        Err(reason) => {
            refuse(client, conn, peer, reason.as_str(), tee, stats);
            return;
        }
    };
    if first.intent != 1 && first.protocol as u32 != config.protocol {
        refuse(client, conn, peer, "protocol", tee, stats);
        return;
    }
    // The gate's read timeout must not follow the connection into play.
    let _ = client.set_read_timeout(None);

    let upstream = match TcpStream::connect_timeout(&config.upstream, UPSTREAM_CONNECT_TIMEOUT) {
        Ok(upstream) => upstream,
        Err(error) => {
            log::warn!("forwarder: conn {conn} cannot reach upstream: {error}");
            refuse(client, conn, peer, "upstream", tee, stats);
            return;
        }
    };
    let _ = upstream.set_nodelay(true);
    let _ = upstream.set_nonblocking(false);
    let upstream = Arc::new(upstream);
    // The guard is the same `Arc` the copy threads use, so the panic path below shuts down the
    // socket the OS actually connected.
    *upstream_guard = Some(Arc::clone(&upstream));
    #[cfg(test)]
    if config.panic_in_handle_client {
        panic!("injected handle_client panic");
    }

    // Buffered handshake bytes (and anything a pipelining client sent alongside) go first,
    // unchanged, before the two copy threads take over.
    let buffered = first.buffered;
    if !buffered.is_empty() {
        let ts = now_ns();
        let mut upstream_out = &*upstream;
        if let Err(error) = upstream_out.write_all(&buffered) {
            log::warn!("forwarder: conn {conn} cannot forward the handshake: {error}");
            close_pair(client, &upstream);
            finish(conn, 0, 0, tee, stats);
            return;
        }
        tee.forwarded(conn, ts, Kind::Serverbound, &buffered);
    }

    let c2s_tee = tee.clone();
    let (c2s_client, c2s_upstream) = (Arc::clone(client), Arc::clone(&upstream));
    #[cfg(test)]
    let fail_half_close = config.fail_half_close;
    let c2s = thread::Builder::new()
        .name(format!("conn-{conn}-c2s"))
        .spawn(move || {
            forward_dir(
                &c2s_client,
                &c2s_upstream,
                conn,
                Kind::Serverbound,
                c2s_tee,
                #[cfg(test)]
                fail_half_close,
            )
        });

    let s2c_tee = tee.clone();
    let (s2c_client, s2c_upstream) = (Arc::clone(client), Arc::clone(&upstream));
    let s2c = thread::Builder::new()
        .name(format!("conn-{conn}-s2c"))
        .spawn(move || {
            forward_dir(
                &s2c_upstream,
                &s2c_client,
                conn,
                Kind::Clientbound,
                s2c_tee,
                #[cfg(test)]
                fail_half_close,
            )
        });

    let (serverbound, clientbound) = match (c2s, s2c) {
        (Ok(c2s), Ok(s2c)) => {
            let c2s = c2s.join().unwrap_or(0);
            let s2c = s2c.join().unwrap_or(0);
            (buffered.len() as u64 + c2s, s2c)
        }
        (c2s, s2c) => {
            log::warn!("forwarder: conn {conn} could not start both copy threads");
            if let Ok(c2s) = c2s {
                let _ = c2s.join();
            }
            if let Ok(s2c) = s2c {
                let _ = s2c.join();
            }
            (buffered.len() as u64, 0)
        }
    };
    finish(conn, serverbound, clientbound, tee, stats);
}

/// One direction of a game connection. On EOF only the write side of the peer is closed, so a
/// half-closing server can still deliver its response. Any error closes both directions, and so
/// does a half-close the platform refuses: the peer can never learn that this direction ended.
///
/// Both sockets are borrowed from the connection's `Arc`s: one OS handle per socket, shared by
/// both directions (`impl Read`/`impl Write` for `&TcpStream`).
///
/// `fail_half_close` is test-only fault injection; it reports the error that Windows really
/// returns (WSAENOTCONN) for some duplicated handles, so the fallback can be tested at all.
fn forward_dir(
    mut from: &TcpStream,
    mut to: &TcpStream,
    conn: u32,
    kind: Kind,
    tee: Tee,
    #[cfg(test)] fail_half_close: bool,
) -> u64 {
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut forwarded: u64 = 0;
    loop {
        match from.read(&mut buffer) {
            Ok(0) => {
                // The half-close is the only signal that this direction is finished, and the
                // peer answers it with its own EOF. If it cannot be delivered the peer never
                // learns about it and would wait forever, so the connection is torn down: it is
                // broken either way, and a broken connection must end instead of leaking.
                #[cfg(test)]
                let half_close = if fail_half_close {
                    Err(io::Error::other("injected half-close refusal"))
                } else {
                    to.shutdown(Shutdown::Write)
                };
                #[cfg(not(test))]
                let half_close = to.shutdown(Shutdown::Write);
                if let Err(error) = half_close {
                    log::warn!(
                        "forwarder: conn {conn} cannot half-close the {kind:?} side ({error}); \
                         closing both directions"
                    );
                    close_pair(from, to);
                }
                break;
            }
            Ok(n) => {
                // The record's timestamp is when read() returned, before the write, so a reply
                // can never carry an earlier timestamp than its request (spikes/README.md).
                let ts = now_ns();
                if to.write_all(&buffer[..n]).is_err() {
                    close_pair(from, to);
                    break;
                }
                forwarded += n as u64;
                tee.forwarded(conn, ts, kind, &buffer[..n]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => {
                close_pair(from, to);
                break;
            }
        }
    }
    forwarded
}

fn close_pair(a: &TcpStream, b: &TcpStream) {
    let _ = a.shutdown(Shutdown::Both);
    let _ = b.shutdown(Shutdown::Both);
}

fn refuse(
    client: &TcpStream,
    conn: u32,
    peer: &str,
    reason: &'static str,
    tee: &Tee,
    stats: &Stats,
) {
    log::warn!("forwarder: conn {conn} {peer} refused: {reason}");
    stats.send(Event::Refused {
        conn,
        peer: peer.to_string(),
        reason,
    });
    tee.closed(conn);
    let _ = client.shutdown(Shutdown::Both);
}

fn finish(conn: u32, serverbound: u64, clientbound: u64, tee: &Tee, stats: &Stats) {
    tee.closed(conn);
    stats.send(Event::Closed {
        conn,
        serverbound,
        clientbound,
    });
}

#[cfg(test)]
mod tests;

//! The server side of the body port.
//!
//! One accept thread owns the listener and one connection thread serves the
//! single active brain. The driver's tick thread only touches atomics, a
//! bounded channel and the snapshot slot; it never waits for the network.

use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use praxion_eventlog::Event;

use crate::frame::{FrameError, FrameReader, write_frame};
use crate::wire::{BodyMessage, BrainMessage, ByeReason, Capability, PORT_VERSION, Snapshot};

/// Capacity of the outgoing queue: events the driver may run ahead of the
/// network thread (proposal §2.5).
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// Capacity of the incoming queue: brain messages waiting for `poll_incoming`.
const INCOMING_CAPACITY: usize = 256;

/// How long the accept thread sleeps between non-blocking accepts.
const ACCEPT_POLL: Duration = Duration::from_millis(2);

/// How long a connection thread waits for input before servicing the outgoing
/// side. The body tick is 50 ms, so 5 ms adds no meaningful latency.
const READ_POLL: Duration = Duration::from_millis(5);

/// How long the brain has to answer the body's `hello`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// A stuck brain must not wedge the connection thread forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// What the driver tells the brain about itself in `hello`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortConfig {
    /// For example `"praxion 0.x (abc123)"`.
    pub body: String,
    /// The run id the event log uses.
    pub run_id: String,
}

/// Whether a brain is connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// No connection; the listener is waiting.
    Disconnected,
    /// A socket is accepted and the body's `hello` is sent; the brain's
    /// `hello` has not arrived yet.
    Handshaking,
    /// The handshake completed; events flow.
    Connected,
}

impl ConnectionState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            1 => ConnectionState::Handshaking,
            2 => ConnectionState::Connected,
            _ => ConnectionState::Disconnected,
        }
    }

    const fn as_raw(self) -> u8 {
        match self {
            ConnectionState::Disconnected => 0,
            ConnectionState::Handshaking => 1,
            ConnectionState::Connected => 2,
        }
    }
}

/// What [`PortServer::publish`] did with an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The event is in the queue and will be sent in `seq` order.
    Queued,
    /// The queue was full. The event is gone; a `dropped(n)` and a fresh
    /// snapshot will follow once the driver publishes one.
    Dropped,
}

/// A snapshot of the port's counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortStats {
    /// Events discarded since the last connection or the last `dropped(n)`.
    pub dropped: u64,
    /// True when the brain must be resynchronised: the driver should call
    /// [`PortServer::publish_snapshot`] with a fresh state.
    pub needs_snapshot: bool,
}

/// The listening port plus the driver-facing handles.
///
/// The server listens on the address given to [`PortServer::bind`]. The brain
/// connects, receives the body's `hello`, answers with its own `hello`, and
/// then receives a `snapshot` if one has been published. A newer connection
/// replaces the old one, which gets `bye { reason: "replaced" }` (proposal
/// §2.1).
///
/// Dropping the server asks the connection thread to send
/// `bye { reason: "shutdown" }`, then joins both threads.
pub struct PortServer {
    local_addr: SocketAddr,
    outbound: SyncSender<Event>,
    incoming: Mutex<Receiver<BrainMessage>>,
    shared: Arc<Shared>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PortServer {
    /// Bind `addr` and start the network thread.
    ///
    /// Pass `127.0.0.1:0` to let the OS choose the port;
    /// [`PortServer::local_addr`] reports it.
    pub fn bind(addr: SocketAddr, config: PortConfig) -> io::Result<Self> {
        Self::bind_with_capacity(addr, config, DEFAULT_QUEUE_CAPACITY)
    }

    /// Like [`PortServer::bind`] with an explicit outgoing-queue capacity.
    ///
    /// The capacity is the number of events the driver may run ahead of the
    /// network. A capacity of zero is a rendezvous channel: every publish
    /// overflows unless the network thread is waiting at that instant. Tests
    /// use it to exercise overflow deterministically.
    pub fn bind_with_capacity(
        addr: SocketAddr,
        config: PortConfig,
        capacity: usize,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let local_addr = listener.local_addr()?;
        let (outbound, outbound_rx) = sync_channel(capacity);
        let (incoming_tx, incoming) = sync_channel(INCOMING_CAPACITY);
        let shared = Arc::new(Shared::new());
        let shutdown = Arc::new(AtomicBool::new(false));
        // One connection thread at a time; the mutex lets the next connection
        // take the same queue after the previous one exits.
        let outbound_rx = Arc::new(Mutex::new(outbound_rx));

        let thread = {
            let shared = Arc::clone(&shared);
            let shutdown = Arc::clone(&shutdown);
            thread::Builder::new()
                .name("praxion-port-accept".to_owned())
                .spawn(move || {
                    accept_loop(listener, config, shared, outbound_rx, incoming_tx, shutdown);
                })?
        };

        Ok(Self {
            local_addr,
            outbound,
            incoming: Mutex::new(incoming),
            shared,
            shutdown,
            thread: Some(thread),
        })
    }

    /// The address the brain should connect to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Drain the brain messages received since the last call.
    ///
    /// Never blocks. The result holds `command` and `pop` messages only; the
    /// handshake, `ping` and `resync_request` are handled by the port itself.
    pub fn poll_incoming(&self) -> Vec<BrainMessage> {
        self.incoming
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .try_iter()
            .collect()
    }

    /// When the brain last sent anything, including a `ping`.
    ///
    /// The driver logs `brain_silent` after the 3 s silence rule (proposal
    /// §2.5). `None` means no brain has completed the handshake yet.
    pub fn last_brain_message(&self) -> Option<Instant> {
        *self
            .shared
            .last_brain
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// The body's current tick, stored every tick: `pong` answers with it, so
    /// the brain's expiry arithmetic stays right while no event is published
    /// (an idle body between goals left it minutes behind and every command
    /// expired before it started, autonomous run 2026-11-08).
    pub fn set_tick(&self, tick: u64) {
        self.shared.latest_tick.store(tick, Ordering::Relaxed);
    }

    /// Ask the connection thread to send `bye { reason: "shutdown" }` and stop
    /// accepting. Idempotent; dropping the server still joins the threads.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Queue one event-log event. Never waits for the network.
    ///
    /// The wire `seq` is the event's log `seq`; no second counter exists. The
    /// fast path is one `try_send`. A full queue records the drop under a short
    /// lock (the connection thread holds it only to swap counters) and returns
    /// [`PublishOutcome::Dropped`].
    pub fn publish(&self, event: Event) -> PublishOutcome {
        self.shared
            .latest_seq
            .fetch_max(event.seq, Ordering::Relaxed);
        self.shared.latest_tick.store(event.tick, Ordering::Relaxed);
        match self.outbound.try_send(event) {
            Ok(()) => PublishOutcome::Queued,
            Err(TrySendError::Full(dropped)) => {
                // With no brain connected there is nobody to resynchronise:
                // recording the drop would leave `needs_snapshot` true forever
                // and rebuild a snapshot on every tick (review of A2-A3, 7).
                if self.connection_state() == ConnectionState::Connected {
                    self.shared.record_drop(dropped.seq);
                }
                PublishOutcome::Dropped
            }
            // The network thread is gone: the server is shutting down.
            Err(TrySendError::Disconnected(_)) => PublishOutcome::Dropped,
        }
    }

    /// Replace the latest snapshot and ask the connection thread to send it.
    ///
    /// The driver calls this on connect, every wake, after an accepted or
    /// settled command, and whenever [`PortServer::needs_snapshot`] is true.
    /// `snapshot.seq` must be the event-log `seq` the state reflects.
    pub fn publish_snapshot(&self, snapshot: Snapshot) {
        let mut slot = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = Some(snapshot);
        drop(slot);
        self.shared.snapshot_owed.store(true, Ordering::Release);
    }

    /// The current connection state.
    pub fn connection_state(&self) -> ConnectionState {
        ConnectionState::from_raw(self.shared.state.load(Ordering::Acquire))
    }

    /// True when events were dropped or the brain asked for a resync, and the
    /// driver must publish a fresh snapshot at or after the drop point.
    pub fn needs_snapshot(&self) -> bool {
        self.shared.needs_snapshot()
    }

    /// The port's counters.
    pub fn stats(&self) -> PortStats {
        PortStats {
            dropped: self.shared.dropped(),
            needs_snapshot: self.shared.needs_snapshot(),
        }
    }
}

impl Drop for PortServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Overflow and resync bookkeeping, in one lock so a drop that races with a
/// snapshot cannot be lost. The driver touches it only on a failed `try_send`
/// (and once per tick for `stats`), never on the queued fast path.
struct Pending {
    /// Events discarded since the last `dropped(n)`.
    dropped: u64,
    /// Smallest snapshot `seq` that may follow: the highest discarded event
    /// `seq`, or the last event the brain saw after a `resync_request`.
    floor: u64,
    /// A `dropped(n)` and/or snapshot must be sent.
    resync: bool,
}

/// State shared by the driver thread, the accept thread and the connection
/// thread.
struct Shared {
    snapshot: Mutex<Option<Snapshot>>,
    pending: Mutex<Pending>,
    latest_seq: AtomicU64,
    latest_tick: AtomicU64,
    snapshot_owed: AtomicBool,
    state: AtomicU8,
    /// When the brain last sent anything. The driver reads it for the silence
    /// rule; `None` until a handshake completed.
    last_brain: Mutex<Option<Instant>>,
}

impl Shared {
    fn new() -> Self {
        Self {
            snapshot: Mutex::new(None),
            pending: Mutex::new(Pending {
                dropped: 0,
                floor: 0,
                resync: false,
            }),
            latest_seq: AtomicU64::new(0),
            latest_tick: AtomicU64::new(0),
            snapshot_owed: AtomicBool::new(false),
            state: AtomicU8::new(ConnectionState::Disconnected.as_raw()),
            last_brain: Mutex::new(None),
        }
    }

    /// Any message from the brain counts as liveness (proposal §2.5).
    fn note_brain_message(&self) {
        let mut last = self
            .last_brain
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *last = Some(Instant::now());
    }

    fn latest_snapshot(&self) -> Option<Snapshot> {
        self.snapshot
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Record that an event was discarded. The next snapshot must be at or
    /// after this event's `seq` for the brain to recover.
    fn record_drop(&self, seq: u64) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        pending.dropped += 1;
        pending.floor = pending.floor.max(seq);
        pending.resync = true;
    }

    /// A `resync_request` cuts at the last event the brain has seen.
    fn request_resync(&self, last_sent: u64) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        pending.floor = pending.floor.max(last_sent);
        pending.resync = true;
    }

    fn needs_snapshot(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .resync
    }

    fn dropped(&self) -> u64 {
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .dropped
    }

    /// Take the pending drop count and clear the request, but only when
    /// `snapshot_seq` covers every discarded event. `None` means "send
    /// nothing yet; wait for a newer snapshot".
    fn take_resync(&self, snapshot_seq: u64) -> Option<u64> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !pending.resync || snapshot_seq < pending.floor {
            return None;
        }
        let dropped = pending.dropped;
        pending.dropped = 0;
        pending.floor = 0;
        pending.resync = false;
        Some(dropped)
    }

    /// A new connection starts from its own baseline: old drops are covered by
    /// the snapshot the connection opens with.
    fn reset_resync(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *pending = Pending {
            dropped: 0,
            floor: 0,
            resync: false,
        };
        self.snapshot_owed.store(false, Ordering::Release);
    }
}

/// Everything a connection thread needs.
struct Session {
    config: PortConfig,
    shared: Arc<Shared>,
    outbound: Arc<Mutex<Receiver<Event>>>,
    incoming: SyncSender<BrainMessage>,
    shutdown: Arc<AtomicBool>,
    replaced: Arc<AtomicBool>,
}

/// One live connection thread plus the flag that asks it to step aside.
struct Connection {
    handle: JoinHandle<()>,
    replaced: Arc<AtomicBool>,
}

fn accept_loop(
    listener: TcpListener,
    config: PortConfig,
    shared: Arc<Shared>,
    outbound: Arc<Mutex<Receiver<Event>>>,
    incoming: SyncSender<BrainMessage>,
    shutdown: Arc<AtomicBool>,
) {
    if listener.set_nonblocking(true).is_err() {
        shutdown.store(true, Ordering::Release);
        return;
    }

    let mut connection: Option<Connection> = None;
    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        if connection
            .as_ref()
            .is_some_and(|live| live.handle.is_finished())
            && let Some(finished) = connection.take()
        {
            let _ = finished.handle.join();
        }
        match listener.accept() {
            Ok((stream, _)) => {
                // A newer brain replaces the old one (proposal §2.1). The old
                // thread gets `bye { reason: "replaced" }` and is joined before
                // the new one starts, so the two never share the event queue.
                if let Some(old) = connection.take() {
                    old.replaced.store(true, Ordering::Release);
                    let _ = old.handle.join();
                }
                let replaced = Arc::new(AtomicBool::new(false));
                let session = Session {
                    config: config.clone(),
                    shared: Arc::clone(&shared),
                    outbound: Arc::clone(&outbound),
                    incoming: incoming.clone(),
                    shutdown: Arc::clone(&shutdown),
                    replaced: Arc::clone(&replaced),
                };
                // A failed spawn leaves the listener free for the next
                // attempt; the socket closes with the dropped stream.
                if let Ok(handle) = thread::Builder::new()
                    .name("praxion-port-conn".to_owned())
                    .spawn(move || serve_connection(stream, session))
                {
                    connection = Some(Connection { handle, replaced });
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
        thread::sleep(ACCEPT_POLL);
    }

    shutdown.store(true, Ordering::Release);
    if let Some(live) = connection.take() {
        let _ = live.handle.join();
    }
}

fn serve_connection(stream: TcpStream, session: Session) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(READ_POLL));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    session
        .shared
        .state
        .store(ConnectionState::Handshaking.as_raw(), Ordering::Release);

    let _ = pump(stream, &session);

    session
        .shared
        .state
        .store(ConnectionState::Disconnected.as_raw(), Ordering::Release);
}

fn pump(stream: TcpStream, session: &Session) -> Result<(), FrameError> {
    let read_half = stream.try_clone()?;
    let mut reader = FrameReader::new(read_half);
    let mut writer = stream;

    // A new connection starts from its own baseline. Reset before the brain can
    // observe the `hello`: a drop recorded right after it must not be erased by
    // the reset.
    session.shared.reset_resync();

    // The body speaks first (proposal §2.1).
    write_frame(
        &mut writer,
        &BodyMessage::Hello {
            port_version: PORT_VERSION,
            caps: vec![
                Capability::Command,
                Capability::Snapshot,
                Capability::Pop,
                Capability::Say,
            ],
            body: session.config.body.clone(),
            run_id: session.config.run_id.clone(),
        },
    )?;

    let handshake_started = Instant::now();
    let first = loop {
        if session.replaced.load(Ordering::Acquire) {
            let _ = write_frame(
                &mut writer,
                &BodyMessage::Bye {
                    reason: ByeReason::Replaced,
                },
            );
            return Ok(());
        }
        match reader.read_json::<BrainMessage>() {
            Ok(Some(message)) => break message,
            Ok(None) => return Ok(()),
            Err(error) if error.is_incomplete() => {
                if handshake_started.elapsed() >= HANDSHAKE_TIMEOUT {
                    let _ = write_frame(
                        &mut writer,
                        &BodyMessage::Bye {
                            reason: ByeReason::Protocol,
                        },
                    );
                    return Ok(());
                }
            }
            Err(_) => {
                let _ = write_frame(
                    &mut writer,
                    &BodyMessage::Bye {
                        reason: ByeReason::Protocol,
                    },
                );
                return Ok(());
            }
        }
    };

    match first {
        BrainMessage::Hello { port_version } if port_version == PORT_VERSION => {}
        BrainMessage::Hello { .. } => {
            let _ = write_frame(
                &mut writer,
                &BodyMessage::Bye {
                    reason: ByeReason::Version,
                },
            );
            return Ok(());
        }
        _ => {
            let _ = write_frame(
                &mut writer,
                &BodyMessage::Bye {
                    reason: ByeReason::Protocol,
                },
            );
            return Ok(());
        }
    }

    session.shared.note_brain_message();
    let initial = session.shared.latest_snapshot();
    let mut sent_seq = None;
    if let Some(snapshot) = initial {
        write_frame(&mut writer, &BodyMessage::Snapshot(snapshot.clone()))?;
        sent_seq = Some(snapshot.seq);
    }
    session
        .shared
        .state
        .store(ConnectionState::Connected.as_raw(), Ordering::Release);

    loop {
        if session.replaced.load(Ordering::Acquire) {
            let _ = write_frame(
                &mut writer,
                &BodyMessage::Bye {
                    reason: ByeReason::Replaced,
                },
            );
            return Ok(());
        }
        if session.shutdown.load(Ordering::Acquire) {
            let _ = write_frame(
                &mut writer,
                &BodyMessage::Bye {
                    reason: ByeReason::Shutdown,
                },
            );
            return Ok(());
        }

        let mut closed = false;
        loop {
            match reader.read_json::<BrainMessage>() {
                Ok(Some(message)) => {
                    if !handle_brain_message(
                        message,
                        &mut writer,
                        &session.shared,
                        &session.incoming,
                        sent_seq,
                    )? {
                        closed = true;
                        break;
                    }
                }
                Ok(None) => {
                    closed = true;
                    break;
                }
                Err(error) if error.is_incomplete() => break,
                Err(_) => {
                    closed = true;
                    break;
                }
            }
        }
        if closed {
            return Ok(());
        }

        service_snapshot(&mut writer, &session.shared, &mut sent_seq)?;

        if service_events(&mut writer, session, &mut sent_seq)? {
            return Ok(());
        }
    }
}

/// Answer pings and resync requests, and enqueue commands for the driver.
/// Returns `Ok(false)` when the connection must close.
fn handle_brain_message(
    message: BrainMessage,
    writer: &mut TcpStream,
    shared: &Shared,
    incoming: &SyncSender<BrainMessage>,
    last_sent: Option<u64>,
) -> Result<bool, FrameError> {
    shared.note_brain_message();
    match message {
        BrainMessage::Ping => {
            write_frame(
                writer,
                &BodyMessage::Pong {
                    seq: shared.latest_seq.load(Ordering::Relaxed),
                    tick: shared.latest_tick.load(Ordering::Relaxed),
                },
            )?;
        }
        BrainMessage::ResyncRequest => {
            // The snapshot may cut at the last event the brain has seen: the
            // state at that seq plus every later event is a complete view.
            shared.request_resync(last_sent.unwrap_or(0));
        }
        // A repeated hello is harmless.
        BrainMessage::Hello { .. } => {}
        other @ (BrainMessage::Command(_) | BrainMessage::Pop { .. } | BrainMessage::Say { .. }) => {
            if incoming.try_send(other).is_err() {
                // The driver is not draining its inbox: close so the brain
                // notices instead of silently losing directives.
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Send `dropped(n)` and/or a snapshot when one is owed or the connection has
/// to resynchronise. A snapshot that does not cover the discarded events is
/// held back: the driver publishes a fresh one after seeing `needs_snapshot`.
fn service_snapshot(
    writer: &mut TcpStream,
    shared: &Shared,
    sent_seq: &mut Option<u64>,
) -> Result<(), FrameError> {
    let owed = shared.snapshot_owed.swap(false, Ordering::AcqRel);
    if !owed && !shared.needs_snapshot() {
        return Ok(());
    }
    let Some(snapshot) = shared.latest_snapshot() else {
        return Ok(());
    };

    if let Some(dropped) = shared.take_resync(snapshot.seq) {
        if dropped > 0 {
            write_frame(
                writer,
                &BodyMessage::Dropped {
                    seq: shared.latest_seq.load(Ordering::Relaxed),
                    n: dropped,
                },
            )?;
        }
        write_frame(writer, &BodyMessage::Snapshot(snapshot.clone()))?;
        *sent_seq = Some(sent_seq.map_or(snapshot.seq, |sent| sent.max(snapshot.seq)));
    } else if owed && !shared.needs_snapshot() && snapshot.seq >= sent_seq.unwrap_or(0) {
        // A wake snapshot must not rewind the brain below an event it has, and
        // must not swallow an event it has not been sent yet: cut it at the
        // last sent event, so the queued receipts still follow it.
        let mut snapshot = snapshot.clone();
        if let Some(sent) = *sent_seq {
            snapshot.seq = sent;
        }
        write_frame(writer, &BodyMessage::Snapshot(snapshot.clone()))?;
        *sent_seq = Some(sent_seq.map_or(snapshot.seq, |sent| sent.max(snapshot.seq)));
    }
    Ok(())
}

/// Forward queued events in `seq` order. While a resync is pending, queued
/// events are discarded and counted as drops: the coming snapshot covers them.
/// Returns `Ok(true)` when the writer failed.
fn service_events(
    writer: &mut TcpStream,
    session: &Session,
    sent_seq: &mut Option<u64>,
) -> Result<bool, FrameError> {
    let outbound = session
        .outbound
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    if session.shared.needs_snapshot() {
        while let Ok(event) = outbound.try_recv() {
            session.shared.record_drop(event.seq);
        }
        return Ok(false);
    }

    loop {
        match outbound.try_recv() {
            Ok(event) => {
                if sent_seq.is_some_and(|sent| event.seq <= sent) {
                    continue;
                }
                let Event {
                    seq, tick, event, ..
                } = event;
                write_frame(writer, &BodyMessage::Event { seq, tick, event })?;
                *sent_seq = Some(sent_seq.map_or(seq, |sent| sent.max(seq)));
            }
            Err(TryRecvError::Empty) => return Ok(false),
            Err(TryRecvError::Disconnected) => return Ok(true),
        }
    }
}

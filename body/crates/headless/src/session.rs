//! The session: one std thread that owns the socket, its runtime, and the tick loop.
//!
//! The thread runs a single-threaded Tokio runtime because `azalea-protocol`'s
//! `Connection` is Tokio-based: a Tokio `TcpStream` must be driven by the runtime
//! that created it, so login, the play loop and every packet stay on one thread.
//! The owner talks to it over channels only, so the body's threads never touch
//! the socket.
//!
//! With [`ConnectOptions::tee`] the session first subscribes to the forwarder's
//! tee and starts the world feed (decoder + raw store). With
//! [`ConnectOptions::script`] it drives the body from a scripted frame source.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;

use crate::driver::DriverHandle;
use crate::error::Error;
use crate::events::{Command, Event};
use crate::script::Script;
use crate::worldfeed::WorldFeed;
use crate::{login, play};

/// Options for [`Session::connect_with`].
#[derive(Clone, Default)]
pub struct ConnectOptions {
    /// Ask to respawn as soon as the server says we died.
    pub auto_respawn: bool,
    /// Apply the body's loop settings to the session thread: no power throttling,
    /// `HIGH_PRIORITY_CLASS`, time-critical priority, MMCSS task "Games"
    /// (docs/decisions.md 2026-11-01). Windows only; process-local.
    pub tune_loop: bool,
    /// Print every packet the session reads and writes to stderr. For live tests.
    pub trace_packets: bool,
    /// A scripted frame source. Without it the body holds no control at all.
    pub script: Option<Arc<Script>>,
    /// An external decision layer. Takes precedence over `script`.
    pub driver: Option<DriverHandle>,
    /// Subscribe to the forwarder's tee at this address and run the decoder and
    /// world in-process. Without it there is no world and movement is refused.
    pub tee: Option<SocketAddr>,
}

impl std::fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectOptions")
            .field("auto_respawn", &self.auto_respawn)
            .field("tune_loop", &self.tune_loop)
            .field("trace_packets", &self.trace_packets)
            .field("script", &self.script.is_some())
            .field("driver", &self.driver.is_some())
            .field("tee", &self.tee)
            .finish()
    }
}

/// Counters for the owner, written by the session thread.
///
/// Every value is a monotonic counter; a snapshot is meaningful at any time and
/// exact once the session has ended.
#[derive(Debug, Default)]
pub struct SessionStats {
    /// Client ticks the tick loop ran.
    pub ticks_ran: AtomicU64,
    /// Frames handed to the actuator (script steps included).
    pub frames: AtomicU64,
    /// Frames the actuator refused (stale generation, wrong tick, menus).
    pub frames_dropped: AtomicU64,
    /// Ticks physics actually stepped.
    pub physics_ticks: AtomicU64,
    /// Ticks where a control that moves us was held without a usable world.
    pub refused_moves: AtomicU64,
    pub attack_clicks: AtomicU64,
    pub use_clicks: AtomicU64,
    pub input_packets: AtomicU64,
    pub sprint_commands: AtomicU64,
    pub position_packets: AtomicU64,
    pub tick_end_packets: AtomicU64,
    /// `player_position` packets applied, including the spawn teleport.
    pub teleports: AtomicU64,
    /// Real-client shadow mode only: frames the decision loop computed and threw
    /// away, of which some asked for something (a held key, a look change, a
    /// click, a slot, a menu action or chat). The pair says how much of the
    /// operator's play the body would have wanted to control, without any hand
    /// acting on it.
    pub shadow_wanted: AtomicU64,
    /// Hands mode only: frames translated and written to the helper.
    pub hands_frames: AtomicU64,
    /// Hands mode only: frames the helper refused.
    pub hands_refused: AtomicU64,
    /// Hands mode only: frames that asked for a control S3 cannot post (sneak,
    /// sprint, a menu action, chat).
    pub hands_unsupported: AtomicU64,
    /// Hands mode only: takeovers (operator input, focus loss, helper bye or
    /// exit) that stopped the frames.
    pub hands_takeovers: AtomicU64,
    /// The last position physics produced, as three `f64::to_bits` values.
    position: [AtomicU64; 3],
    /// The first position physics produced (the spawn position).
    origin: [AtomicU64; 3],
    has_position: AtomicBool,
    has_origin: AtomicBool,
    /// One line about the world feed when the session ends.
    world: std::sync::Mutex<Option<String>>,
}

impl SessionStats {
    /// Record a one-line summary of the world feed (status, chunks, warnings).
    pub(crate) fn set_world_summary(&self, summary: String) {
        let mut world = self
            .world
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *world = Some(summary);
    }

    /// The world feed's final state, if a world feed was running.
    pub fn world_summary(&self) -> Option<String> {
        self.world
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    /// `player_position` packets after the first (spawn) one: server corrections.
    pub fn corrections(&self) -> u64 {
        self.teleports
            .load(Ordering::Relaxed)
            .saturating_sub(1)
    }

    /// Record where physics says we are.
    pub(crate) fn set_position(&self, position: azalea_core::position::Vec3) {
        for (counter, value) in self.position.iter().zip([position.x, position.y, position.z]) {
            counter.store(value.to_bits(), Ordering::Relaxed);
        }
        self.has_position.store(true, Ordering::Relaxed);
        if !self.has_origin.swap(true, Ordering::Relaxed) {
            for (counter, value) in self.origin.iter().zip([position.x, position.y, position.z]) {
                counter.store(value.to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// The last position physics produced, if physics has run.
    pub fn position(&self) -> Option<azalea_core::position::Vec3> {
        if !self.has_position.load(Ordering::Relaxed) {
            return None;
        }
        Some(self.read(&self.position))
    }

    /// The first position physics produced, if physics has run.
    pub fn origin(&self) -> Option<azalea_core::position::Vec3> {
        if !self.has_origin.load(Ordering::Relaxed) {
            return None;
        }
        Some(self.read(&self.origin))
    }

    fn read(&self, values: &[AtomicU64; 3]) -> azalea_core::position::Vec3 {
        azalea_core::position::Vec3::new(
            f64::from_bits(values[0].load(Ordering::Relaxed)),
            f64::from_bits(values[1].load(Ordering::Relaxed)),
            f64::from_bits(values[2].load(Ordering::Relaxed)),
        )
    }

    fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }

    /// A one-block summary for the binary's log.
    pub fn summary(&self) -> String {
        format!(
            "ticks: {}  physics: {}  frames: {}  dropped: {}  refused moves: {}\n\
             packets: input {}, sprint {}, position {}, tick_end {}\n\
             teleports: {} (corrections after spawn: {})\n\
             clicks: attack {}, use {}\n\
             shadow: {} of {} frames asked for something (real-client mode only)\n\
             hands: {} sent, {} refused, {} unsupported, {} takeovers",
            Self::get(&self.ticks_ran),
            Self::get(&self.physics_ticks),
            Self::get(&self.frames),
            Self::get(&self.frames_dropped),
            Self::get(&self.refused_moves),
            Self::get(&self.input_packets),
            Self::get(&self.sprint_commands),
            Self::get(&self.position_packets),
            Self::get(&self.tick_end_packets),
            Self::get(&self.teleports),
            self.corrections(),
            Self::get(&self.attack_clicks),
            Self::get(&self.use_clicks),
            Self::get(&self.shadow_wanted),
            Self::get(&self.frames),
            Self::get(&self.hands_frames),
            Self::get(&self.hands_refused),
            Self::get(&self.hands_unsupported),
            Self::get(&self.hands_takeovers),
        )
    }
}

/// A connected, logged-in client.
pub struct Session {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
    connected: Arc<AtomicBool>,
    stats: Arc<SessionStats>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("connected", &self.is_connected())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Connect and log in with default options.
    pub fn connect(addr: SocketAddr, username: &str) -> Result<Self, Error> {
        Self::connect_with(addr, username, ConnectOptions::default())
    }

    /// Connect and log in.
    ///
    /// Blocks until the server has us in play state, so a server that refuses the
    /// login is reported here as an error rather than as an event.
    pub fn connect_with(
        addr: SocketAddr,
        username: &str,
        options: ConnectOptions,
    ) -> Result<Self, Error> {
        let username = username.to_owned();
        let (command_tx, command_rx) = unbounded_channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let connected = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(SessionStats::default());

        let thread_connected = Arc::clone(&connected);
        let thread_stats = Arc::clone(&stats);
        let thread_name = format!("praxion-headless {username}");
        let thread = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            let _ = ready_tx.send(Err(Error::Io(error)));
                            thread_connected.store(false, Ordering::Relaxed);
                            return;
                        }
                    };
                runtime.block_on(run_session(
                    addr,
                    username,
                    options,
                    command_rx,
                    event_tx,
                    ready_tx,
                    thread_connected,
                    thread_stats,
                ));
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands: command_tx,
                events: event_rx,
                thread: Some(thread),
                connected,
                stats,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(Error::ThreadEnded)
            }
        }
    }

    /// Ask the session to close the connection. The `Disconnected` event follows.
    pub fn disconnect(&self) {
        let _ = self.commands.send(Command::Disconnect);
    }

    /// Wait at most `timeout` for the next event. `None` means nothing arrived.
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Whether the session thread is still running.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// The counters of this session.
    pub fn stats(&self) -> &SessionStats {
        &self.stats
    }

    /// Disconnect and wait for the session thread to end.
    pub fn close(mut self) {
        self.disconnect();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Dropping the command sender ends the session thread at its next loop
        // iteration. No join here: a thread blocked on a socket write must not be
        // able to hang its owner. `close` is the joining version.
    }
}

/// What a process needs from a running body, shared by [`Session`] (a logged-in
/// body) and [`crate::ShadowSession`] (the operator's own client in shadow
/// mode), so the binary can treat both alike.
pub trait Host {
    /// Wait at most `timeout` for the next event. `None` means nothing arrived.
    fn recv_event_timeout(&self, timeout: Duration) -> Option<Event>;

    /// Whether the host thread is still running.
    fn is_connected(&self) -> bool;

    /// The counters of this host.
    fn stats(&self) -> &SessionStats;

    /// Ask the host to end. The `Disconnected` event follows.
    fn disconnect(&self);
}

impl Host for Session {
    fn recv_event_timeout(&self, timeout: Duration) -> Option<Event> {
        Session::recv_event_timeout(self, timeout)
    }

    fn is_connected(&self) -> bool {
        Session::is_connected(self)
    }

    fn stats(&self) -> &SessionStats {
        Session::stats(self)
    }

    fn disconnect(&self) {
        Session::disconnect(self);
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    addr: SocketAddr,
    username: String,
    options: ConnectOptions,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    events: Sender<Event>,
    ready: Sender<Result<(), Error>>,
    connected: Arc<AtomicBool>,
    stats: Arc<SessionStats>,
) {
    if options.tune_loop {
        crate::win::tune_current_thread();
    }

    // Subscribe to the tee before the game connection so no record is missed.
    let world = match options.tee {
        None => None,
        Some(tee) => match WorldFeed::connect(tee) {
            Ok(feed) => Some(feed),
            Err(message) => {
                let _ = ready.send(Err(Error::Setup(message)));
                connected.store(false, Ordering::Relaxed);
                return;
            }
        },
    };

    let conn = match login::connect(addr, &username, options.trace_packets).await {
        Ok(conn) => conn,
        Err(error) => {
            let _ = ready.send(Err(error));
            connected.store(false, Ordering::Relaxed);
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        // The owner went away while we were logging in.
        connected.store(false, Ordering::Relaxed);
        return;
    }

    let mut context = play::Context {
        commands: &mut commands,
        events: &events,
        auto_respawn: options.auto_respawn,
        trace: options.trace_packets,
        script: options.script,
        driver: options.driver,
        stats: &stats,
    };
    if let Err(error) = play::run(conn, &mut context, world).await {
        let _ = events.send(Event::Disconnected(error.to_string()));
    }
    connected.store(false, Ordering::Relaxed);
}

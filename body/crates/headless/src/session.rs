//! The session: one std thread that owns the socket, its runtime, and the tick loop.
//!
//! The thread runs a single-threaded Tokio runtime because `azalea-protocol`'s
//! `Connection` is Tokio-based: a Tokio `TcpStream` must be driven by the runtime
//! that created it, so login, the play loop and every packet stay on one thread.
//! The owner talks to it over channels only, so the body's threads never touch
//! the socket.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use azalea_protocol::packets::game::ServerboundGamePacket;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;

use crate::error::Error;
use crate::events::{Command, Event};
use crate::{login, play};

/// Options for [`connect_with`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectOptions {
    /// Ask to respawn as soon as the server says we died.
    pub auto_respawn: bool,
    /// Apply the body's loop settings to the session thread: no power throttling,
    /// `HIGH_PRIORITY_CLASS`, time-critical priority, MMCSS task "Games"
    /// (docs/decisions.md 2026-11-01). Windows only; process-local.
    pub tune_loop: bool,
    /// Print every packet the session reads and writes to stderr. For live tests.
    pub trace_packets: bool,
}

/// A connected, logged-in client.
pub struct Session {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
    connected: Arc<AtomicBool>,
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

        let thread_connected = Arc::clone(&connected);
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
                ));
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands: command_tx,
                events: event_rx,
                thread: Some(thread),
                connected,
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

    /// Send a packet to the server. Only valid in play state.
    pub fn send(&self, packet: ServerboundGamePacket) -> Result<(), Error> {
        self.commands
            .send(Command::Send(packet))
            .map_err(|_| Error::LocalDisconnect)
    }

    /// Ask the session to close the connection. The `Disconnected` event follows.
    pub fn disconnect(&self) {
        let _ = self.commands.send(Command::Disconnect);
    }

    /// The next event, if one is already waiting.
    pub fn try_recv_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Block until the next event, or `None` when the session is gone.
    pub fn recv_event(&self) -> Option<Event> {
        self.events.recv().ok()
    }

    /// Wait at most `timeout` for the next event. `None` means nothing arrived.
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<Event> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Whether the session thread is still running.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
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

#[allow(clippy::too_many_arguments)]
async fn run_session(
    addr: SocketAddr,
    username: String,
    options: ConnectOptions,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    events: Sender<Event>,
    ready: Sender<Result<(), Error>>,
    connected: Arc<AtomicBool>,
) {
    if options.tune_loop {
        crate::win::tune_current_thread();
    }

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
    };
    if let Err(error) = play::run(conn, &mut context).await {
        let _ = events.send(Event::Disconnected(error.to_string()));
    }
    connected.store(false, Ordering::Relaxed);
}

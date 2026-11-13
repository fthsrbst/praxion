//! Shadow mode: the decision loop on the operator's own client, without a login
//! and without acting.
//!
//! The operator plays with the vanilla client through the forwarder; the body
//! only watches the tee. The world mirror already follows that client's own
//! serverbound packets: its position, rotation, on-ground flag and a tick
//! counter that advances on every `client_tick_end` it sends. So the body runs
//! the same [`Driver`] as a live session, once per *observed* client tick, and
//! throws every [`ControlFrame`] the driver returns away: nothing is sent to the
//! server and no OS input is posted.
//!
//! The mirror's own state also carries the operator's life: a health at or below
//! zero is reported once as `Event::Died`, exactly as it is for a logged-in
//! body, and a respawn resets the observed body to a new generation.
//!
//! ```text
//! tee -> WorldFeed -> World (the client's own state)
//!                        | Body::observe, once per observed client tick
//!                        v
//!              driver.frame(&TickView) -> ControlFrame -> dropped
//! ```
//!
//! This is step S2 of `docs/proposals/2026-11-12-macos-real-client-body.md`: its
//! purpose is to prove the live tick path, so perception, the port snapshot and
//! the console inventory follow the operator's player.
//!
//! [`ShadowSession::start_with_hands`] is step S3b: the frames are no longer
//! dropped while the helper is armed. Each one becomes a `praxion-hands` line
//! ([`crate::hands::Translator`]) and the client's own packets become the
//! driver's `AppliedReport`, so `move_to` and `mine` can run on the operator's
//! client:
//!
//! ```text
//! driver.frame(&TickView) -> ControlFrame -> Translator -> helper line -> OS input
//! this tick's packets     -> observed_report -> driver.applied(tick, report)
//! ```
//!
//! A takeover, the helper's `BYE` or its death stops the applying: frames are
//! dropped again exactly as in S2, the translator is reset, the body generation
//! moves on (so nothing built for the old control can be applied) and the owner
//! gets `Event::TakenOver`. S3 arms once, with a flag; the console button is S5.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use azalea_block::BlockState;
use azalea_core::position::{BlockPos, Vec3};
use praxion_world::World;

use crate::driver::{DriverHandle, TickView};
use crate::error::Error;
use crate::events::Event;
use crate::frame::{ControlFrame, Held, Look};
use crate::hands::{ClientOptions, Translator, observed_report};
use crate::hands_link::{HandsLink, HandsReply, release_all};
use crate::interact::{AppliedReport, pick};
use crate::physics::Body;
use crate::play::{world_line, world_ready};
use crate::session::{Host, SessionStats};
use crate::worldfeed::{ObservedPacket, WorldFeed};

/// How long the shadow loop waits between two looks at the mirror. A client
/// tick is 50 ms, so 5 ms keeps an observation inside a quarter of a tick
/// without spinning.
const POLL: Duration = Duration::from_millis(5);

/// The most frames one round may drive while it holds the world lock. A long
/// backlog (the chunks around the player arriving after the world became
/// ready, a stalled loop) is caught up over several rounds instead of blocking
/// the world feed thread for hundreds of frames; a capped round does not
/// sleep, so the rest is driven immediately.
const MAX_FRAMES_PER_ROUND: u64 = 20;

/// The per-tick half of a shadow session: the operator's body as the last
/// observation left it, and the counters that make one frame per observed
/// client tick.
///
/// Split from [`ShadowSession`] (which owns the thread) so the tick rule can be
/// driven directly against a hand-built mirror.
pub(crate) struct Shadow {
    body: Body,
    driver: DriverHandle,
    stats: Arc<SessionStats>,
    /// The mirror tick already driven. `World::tick` restarts at zero when a new
    /// connection is followed.
    observed: u64,
    /// The tick number of the next frame, consecutive across connections.
    frame_tick: u64,
    /// The first frame ran, so `Event::Joined` was sent.
    joined: bool,
    /// The mirror's life number as of the last round, `None` before the first
    /// one. `World::life` bumps on every login and respawn.
    life: Option<u64>,
    /// The operator's current life is already reported dead, so a health that
    /// stays at zero is one death, not one per round.
    dead: bool,
    /// The hands, when the session was started with them.
    hands: Option<Hands>,
    /// What the cells the previous tick's packets named held then, so a digest
    /// or a placement can report the state before its own action.
    block_history: HashMap<BlockPos, Option<BlockState>>,
}

impl Shadow {
    fn new(driver: DriverHandle, stats: Arc<SessionStats>) -> Self {
        Self {
            body: Body::new(Vec3::ZERO),
            driver,
            stats,
            observed: 0,
            frame_tick: 0,
            joined: false,
            life: None,
            dead: false,
            hands: None,
            block_history: HashMap::new(),
        }
    }

    /// Watch the mirror's own life: its health and its life number.
    ///
    /// A health at or below zero is the operator's death: the first round that
    /// sees it returns `Event::Died`, and the rounds that follow in the same
    /// life return nothing. The mirror keeps no combat-kill packet, so the
    /// message is `None`. A new life number is a respawn (or a reconnection):
    /// the body is reset the way `PlayState::respawn` resets it, which also
    /// bumps its generation, so frames built against the old life are stale.
    fn notice_life(&mut self, world: &World) -> Option<Event> {
        if self.life.is_some_and(|life| life != world.life()) {
            self.body.respawn();
        }
        self.life = Some(world.life());

        let health = world.health()?;
        if health.health() > 0.0 {
            // A new life's first health update ends the previous death.
            self.dead = false;
            return None;
        }
        if self.dead {
            return None;
        }
        self.dead = true;
        Some(Event::Died { message: None })
    }

    /// Drive up to [`MAX_FRAMES_PER_ROUND`] of the client ticks the mirror saw
    /// since the last call and drop the frames. Returns how many frames were
    /// computed; a longer backlog stays pending for the next round.
    ///
    /// The operator's pose is read once, so a counter that advanced by N (a
    /// stalled loop, a busy batch of server packets) gives N frames for the same
    /// observation with consecutive tick numbers; only the first one gets the
    /// packets, because they are the server's news since the last frame.
    ///
    /// `take_packets` is called only when the round drives at least one frame,
    /// so a round with nothing to drive leaves the packet queue intact for the
    /// frame that consumes it.
    fn observe(
        &mut self,
        world: &World,
        take_packets: impl FnOnce() -> Vec<ObservedPacket>,
    ) -> u64 {
        let mirror_tick = world.tick();
        if mirror_tick < self.observed {
            // The mirror started a new connection: its counter restarted.
            self.observed = 0;
        }
        let Some(position) = world.player_position() else {
            return 0;
        };
        // How many client ticks the operator's pose covers, and how many of them
        // this round drives: the velocity is per tick, so it divides by the
        // whole gap even when the round is capped.
        let pending = mirror_tick.saturating_sub(self.observed);
        let advanced = pending.min(MAX_FRAMES_PER_ROUND);
        if advanced == 0 {
            return 0;
        }

        let (yaw, pitch) = world.player_rotation();
        // A client that has not sent a movement packet yet has not said whether
        // it stands on anything; that reads as "not on the ground", the same
        // default a fresh `Body` has.
        let on_ground = world.player_on_ground().unwrap_or(false);
        self.body.observe(position, yaw, pitch, on_ground, pending);
        self.stats.set_position(position);
        let packets = take_packets();

        // The block cells this tick's packets named, kept for the next tick's
        // report: only the first frame of a round gets the packets.
        let mut named_report: Option<AppliedReport> = None;
        for step in 0..advanced {
            let tick = self.frame_tick;
            self.frame_tick += 1;
            let packet_slice = if step == 0 { &packets[..] } else { &[] };
            let mut driver = self
                .driver
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let frame = driver.frame(&TickView {
                tick,
                body: &self.body,
                world: Some(world),
                packets: packet_slice,
            });
            self.stats.frames.fetch_add(1, Ordering::Relaxed);
            if frame_wants(&frame) {
                self.stats.shadow_wanted.fetch_add(1, Ordering::Relaxed);
            }

            // Shadow mode without hands never calls `applied`: nothing acted on
            // the frame. With hands armed, every frame is written to the helper
            // and the driver learns what the client's own packets did.
            let applying = self.hands.as_ref().is_some_and(|hands| hands.applying);
            if applying {
                let report = observed_report(
                    packet_slice,
                    &|pos| {
                        self.block_history
                            .get(&pos)
                            .copied()
                            .flatten()
                            .or_else(|| world.block_state(pos))
                    },
                    frame.clicks.len(),
                );
                if let Some(hands) = self.hands.as_mut() {
                    let line = hands.translator.line(&frame, yaw, pitch);
                    if line.unsupported {
                        self.stats.hands_unsupported.fetch_add(1, Ordering::Relaxed);
                    }
                    match hands.link.send(&line.encode()) {
                        Ok(()) => {
                            self.stats.hands_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error) => {
                            // The pipe is gone; the next poll reports `Exited`
                            // and the takeover path cleans up.
                            log::warn!("hands: cannot send a frame: {error}");
                            hands.applying = false;
                        }
                    }
                }
                driver.applied(tick, &report);
                if named_report.is_none() {
                    named_report = Some(report);
                }
            }
            drop(driver);
        }

        if let Some(report) = named_report {
            self.remember_block_history(world, &report);
        }

        self.observed += advanced;
        self.stats.ticks_ran.fetch_add(advanced, Ordering::Relaxed);
        advanced
    }

    /// Keep the block states the next tick's report calls "before": the cell
    /// under the crosshair and the cells this tick's packets named, as the
    /// mirror holds them at the end of this tick.
    fn remember_block_history(&mut self, world: &World, report: &AppliedReport) {
        let mut next = HashMap::new();
        if let Some(hit) = pick(world, &self.body).block {
            next.insert(hit.block_pos, world.block_state(hit.block_pos));
        }
        if let Some(pos) = report.block_hit {
            next.insert(pos, world.block_state(pos));
        }
        if let Some((pos, _)) = report.block_before {
            next.insert(pos, world.block_state(pos));
        }
        self.block_history = next;
    }

    /// Poll the helper and handle what it said. Returns the event the owner must
    /// see, if a takeover or the helper's exit stopped the hands.
    fn pump_hands(&mut self) -> Option<Event> {
        let replies = self.hands.as_mut()?.link.poll();
        let mut taken_over = None;
        for reply in replies {
            match reply {
                HandsReply::Ok(_tick) => {}
                HandsReply::Refused(_tick, reason) => {
                    self.stats.hands_refused.fetch_add(1, Ordering::Relaxed);
                    log::warn!("hands: frame refused: {reason}");
                }
                HandsReply::Idle => {
                    log::debug!("hands: the helper released its keys while armed");
                }
                HandsReply::Takeover(reason) => {
                    if let Some(event) = self.stop_hands(reason) {
                        taken_over = Some(event);
                    }
                }
                HandsReply::Bye(reason) => {
                    if let Some(event) = self.stop_hands(format!("helper bye: {reason}")) {
                        taken_over = Some(event);
                    }
                }
                HandsReply::Exited => {
                    if let Some(event) = self.stop_hands("helper exited".to_owned()) {
                        taken_over = Some(event);
                    }
                    if let Some(hands) = &self.hands
                        && let Err(error) = release_all(&hands.helper, hands.client_pid)
                    {
                        log::warn!("hands: release-all failed: {error}");
                    }
                }
            }
        }
        taken_over
    }

    /// Stop applying frames: drop the closed loop, move the body to a new
    /// generation and tell the owner once.
    fn stop_hands(&mut self, reason: String) -> Option<Event> {
        let hands = self.hands.as_mut()?;
        if hands.ended {
            return None;
        }
        hands.ended = true;
        hands.applying = false;
        hands.translator.reset();
        self.body.bump_generation();
        self.stats.hands_takeovers.fetch_add(1, Ordering::Relaxed);
        Some(Event::TakenOver { reason })
    }
}

/// Whether a dropped frame asked for anything: a held key, a look change, a
/// click, a slot, a menu action or chat.
fn frame_wants(frame: &ControlFrame) -> bool {
    frame.held != Held::NONE
        || !matches!(frame.look, Look::None)
        || !frame.clicks.is_empty()
        || frame.select_slot.is_some()
        || frame.menu.is_some()
        || frame.chat.is_some()
}

/// The settings a hands session needs: which helper, which client, and the
/// client's own mouse gain.
#[derive(Clone, Debug)]
pub struct HandsConfig {
    /// The `praxion-hands` binary.
    pub helper: PathBuf,
    /// The client process the helper posts to.
    pub client_pid: u32,
    /// Ask macOS to bring the client forward before arming.
    pub activate: bool,
    /// The client's `options.txt`, parsed: its sensitivity and its bindings.
    pub options: ClientOptions,
    /// Ticks between a posted delta and the client's rotation packet.
    pub look_latency_ticks: usize,
}

/// The running helper and the closed loop the frame path keeps for it.
struct Hands {
    link: HandsLink,
    translator: Translator,
    helper: PathBuf,
    client_pid: u32,
    /// The helper is armed and every frame is applied.
    applying: bool,
    /// A terminal reply was handled, so a later one must not take over twice.
    ended: bool,
}

impl Hands {
    fn spawn(config: HandsConfig) -> Result<Self, Error> {
        let link = HandsLink::spawn(&config.helper, config.client_pid, config.activate)?;
        Ok(Self {
            link,
            translator: Translator::new(
                config.options.gain_degrees_per_pixel(),
                config.look_latency_ticks,
            ),
            helper: config.helper,
            client_pid: config.client_pid,
            applying: true,
            ended: false,
        })
    }
}

/// A shadow session: the decision loop attached to the operator's own client.
///
/// Shaped like [`crate::Session`] so a binary can treat both alike (see
/// [`crate::Host`]), with the difference that nothing is sent unless the session
/// was started with hands: without them every frame the driver returns is
/// dropped.
pub struct ShadowSession {
    events: Receiver<Event>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    connected: Arc<AtomicBool>,
    stats: Arc<SessionStats>,
}

impl std::fmt::Debug for ShadowSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowSession")
            .field("connected", &self.is_connected())
            .finish_non_exhaustive()
    }
}

impl ShadowSession {
    /// Attach to the tee at `tee` and start the observation loop.
    ///
    /// Blocks until the tee stream header arrived, so a game connection opened
    /// afterwards cannot be missed; the operator's own join arrives later as
    /// `Event::Joined`, on the first observed client tick.
    pub fn start(tee: SocketAddr, driver: DriverHandle) -> Result<Self, Error> {
        Self::start_inner(tee, driver, None)
    }

    /// Like [`ShadowSession::start`], but every frame is applied through the
    /// `praxion-hands` helper while it is armed.
    ///
    /// The helper is started (and armed) before the shadow thread begins, so a
    /// helper that cannot arm is reported here instead of silently playing
    /// nothing.
    pub fn start_with_hands(
        tee: SocketAddr,
        driver: DriverHandle,
        hands: HandsConfig,
    ) -> Result<Self, Error> {
        Self::start_inner(tee, driver, Some(hands))
    }

    fn start_inner(
        tee: SocketAddr,
        driver: DriverHandle,
        hands: Option<HandsConfig>,
    ) -> Result<Self, Error> {
        let feed = WorldFeed::connect(tee).map_err(Error::Setup)?;
        // The helper is armed only once the operator is in the world (the
        // first driven tick), not here: armed at start-up it watched the
        // client's loading and first-launch screens, and the operator's click
        // on one of them took over before the bot had played a tick (live
        // recording, 2026-11-13). A helper that cannot arm then ends the
        // session with the reason instead of playing nothing.
        let (event_tx, event_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(SessionStats::default());

        let thread_stop = Arc::clone(&stop);
        let thread_connected = Arc::clone(&connected);
        let thread_stats = Arc::clone(&stats);
        let thread = thread::Builder::new()
            .name("praxion-shadow".to_owned())
            .spawn(move || {
                run(
                    feed,
                    driver,
                    hands,
                    event_tx,
                    thread_stop,
                    thread_connected,
                    thread_stats,
                );
            })?;

        Ok(Self {
            events: event_rx,
            stop,
            thread: Some(thread),
            connected,
            stats,
        })
    }

    /// Ask the thread to stop. The `Disconnected` event follows.
    pub fn disconnect(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Wait at most `timeout` for the next event. `None` means nothing arrived.
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Whether the shadow thread is still running.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// The counters of this session.
    pub fn stats(&self) -> &SessionStats {
        &self.stats
    }

    /// Stop and wait for the shadow thread to end.
    pub fn close(mut self) {
        ShadowSession::disconnect(&self);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ShadowSession {
    fn drop(&mut self) {
        // Without this the detached thread would keep reading the tee after its
        // owner is gone. No join: `close` is the joining version.
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Host for ShadowSession {
    fn recv_event_timeout(&self, timeout: Duration) -> Option<Event> {
        ShadowSession::recv_event_timeout(self, timeout)
    }

    fn is_connected(&self) -> bool {
        ShadowSession::is_connected(self)
    }

    fn stats(&self) -> &SessionStats {
        ShadowSession::stats(self)
    }

    fn disconnect(&self) {
        ShadowSession::disconnect(self)
    }
}

/// The shadow thread: wait for the mirror to be usable, then drive it.
fn run(
    feed: WorldFeed,
    driver: DriverHandle,
    mut pending_hands: Option<HandsConfig>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    stats: Arc<SessionStats>,
) {
    let mut shadow = Shadow::new(driver, Arc::clone(&stats));

    let reason = loop {
        if stop.load(Ordering::Relaxed) {
            break Error::LocalDisconnect.to_string();
        }

        // One look at the mirror. The lock is held while the frames are
        // computed, as in the play loop, so the driver sees one world state.
        // The packet queue is taken under the same lock and in the feed's own
        // lock order (world, then queue), so no record is lost in between.
        let (drove, death, ending) = {
            let world = feed.lock();
            if world.is_ended() {
                (false, None, Some("the client connection ended".to_owned()))
            } else if let Some(reason) = world.status().reason() {
                (
                    false,
                    None,
                    Some(format!("the world mirror became invalid: {reason}")),
                )
            } else {
                // The operator's own life comes first: a death must be reported
                // even when no position is known (a respawn clears it).
                let death = shadow.notice_life(&world);
                let drove = match world.player_position() {
                    Some(position) if world_ready(&world, position) => {
                        let frames = shadow.observe(&world, || feed.take_packets());
                        if frames > 0 && !shadow.joined {
                            shadow.joined = true;
                            let _ = events.send(Event::Joined);
                        }
                        frames > 0
                    }
                    // No pose yet, or the chunks around the player are still
                    // arriving: wait, as the play loop does before its first
                    // step.
                    _ => false,
                };
                (drove, death, None)
            }
        };

        if let Some(reason) = ending {
            break reason;
        }
        if shadow.joined
            && let Some(config) = pending_hands.take()
        {
            match Hands::spawn(config) {
                Ok(hands) => {
                    log::info!("hands: armed once the operator was in the world");
                    shadow.hands = Some(hands);
                }
                Err(error) => break format!("hands: cannot arm: {error}"),
            }
        }
        if let Some(event) = shadow.pump_hands() {
            let _ = events.send(event);
        }
        if let Some(event) = death {
            let _ = events.send(event);
        }
        if !drove {
            thread::sleep(POLL);
        }
    };

    stats.set_world_summary(world_line(&feed.lock()));
    let _ = events.send(Event::Disconnected(reason));
    connected.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests;

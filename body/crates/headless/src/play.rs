//! The play state: apply one frame, step physics once, answer what the protocol
//! requires, stay connected.
//!
//! Per tick, in azalea's order (`azalea-client/src/plugins/movement.rs:57-72`,
//! `.../tick_end.rs:18-36`):
//!
//! ```text
//! ControlFrame -> body (look, held) -> physics step
//!   -> player_input (when it changed) -> sprint command (when it changed)
//!   -> the one move packet vanilla would send -> client_tick_end
//! ```
//!
//! Everything else is event driven: keep alive, ping, chunk batches, teleports,
//! death. A tick without a usable world steps nothing and sends nothing but
//! `client_tick_end`: movement is refused rather than guessed.

use std::sync::Arc;

use azalea_core::entity_id::MinecraftEntityId;
use azalea_core::position::{ChunkPos, Vec3};
use azalea_entity::LookDirection;
use azalea_protocol::connect::WriteConnection;
use azalea_protocol::packets::Packet;
use azalea_protocol::packets::ProtocolPacket;
use azalea_protocol::packets::game::{
    ClientboundGamePacket, ServerboundAcceptTeleportation, ServerboundChunkBatchReceived,
    ServerboundClientCommand, ServerboundClientTickEnd, ServerboundConfigurationAcknowledged,
    ServerboundCookieResponse, ServerboundGamePacket, ServerboundKeepAlive, ServerboundPlayerCommand,
    ServerboundPlayerInput, ServerboundPlayerLoaded, ServerboundPong, s_client_command::Action,
    s_player_command,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use crate::chunks::ChunkBatchInfo;
use crate::config;
use crate::driver::{DriverHandle, TickView};
use crate::error::Error;
use crate::events::{Command, Event};
use crate::frame::{Click, ControlFrame, Held};
use crate::interact::{ActuatorState, AppliedReport, Outgoing};
use crate::login::{self, GameConn};
use crate::movement::Movement;
use crate::physics::Body;
use crate::script::{Runner, Script};
use crate::session::SessionStats;
use crate::worldfeed::WorldFeed;
use crate::TICK;

/// What the owner can tell the session, and where it goes.
pub(crate) struct Context<'a> {
    pub commands: &'a mut UnboundedReceiver<Command>,
    pub events: &'a std::sync::mpsc::Sender<Event>,
    pub auto_respawn: bool,
    pub trace: bool,
    pub script: Option<Arc<Script>>,
    pub driver: Option<DriverHandle>,
    pub stats: &'a Arc<SessionStats>,
}

/// What the session keeps between ticks.
struct PlayState {
    movement: Movement,
    body: Body,
    world: Option<WorldFeed>,
    runner: Option<Runner>,
    driver: Option<DriverHandle>,
    actuator: ActuatorState,
    /// The state id of the open container, from `container_set_content`/`_slot`.
    menu_state_id: u32,
    /// The controls the actuator currently holds.
    held: Held,
    /// The input packet the server already has.
    last_sent_input: ServerboundPlayerInput,
    /// The sprinting flag the server already has.
    last_sprinting: bool,
    chunks: ChunkBatchInfo,
    /// Our own chunk has arrived, so the server may be told we are loaded.
    client_loaded: bool,
    /// Our entity id from the play login packet, used for the sprint command
    /// and to recognise our death.
    own_entity_id: Option<i32>,
    /// We are dead and have not respawned yet; stops repeated respawn requests.
    dead: bool,
    /// The first play packet arrived, so `Event::Joined` has been sent.
    joined: bool,
    auto_respawn: bool,
    /// Client ticks the session has run.
    tick: u64,
    /// A refusal to move has been logged once already.
    refusal_logged: bool,
    /// The end of the script has been logged once.
    script_done_logged: bool,
}

impl PlayState {
    fn new(
        auto_respawn: bool,
        world: Option<WorldFeed>,
        script: Option<Arc<Script>>,
        driver: Option<DriverHandle>,
        position: Vec3,
        look: LookDirection,
    ) -> Self {
        let mut movement = Movement::default();
        movement.teleported(position, look);
        Self {
            movement,
            body: Body::new(position),
            world,
            runner: script.map(|script| Runner::new((*script).clone())),
            driver,
            actuator: ActuatorState::default(),
            menu_state_id: 0,
            held: Held::NONE,
            last_sent_input: ServerboundPlayerInput::default(),
            last_sprinting: false,
            chunks: ChunkBatchInfo::default(),
            client_loaded: false,
            own_entity_id: None,
            dead: false,
            joined: false,
            auto_respawn,
            tick: 0,
            refusal_logged: false,
            script_done_logged: false,
        }
    }

    /// One client tick.
    async fn tick(
        &mut self,
        writer: &mut WriteConnection<ServerboundGamePacket>,
        trace: bool,
        stats: &SessionStats,
    ) -> Result<(), Error> {
        let tick = self.tick;
        self.tick += 1;
        stats.ticks_ran.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let mut stepped = false;
        if self.client_loaded {
            let frame = self.next_frame(tick, stats);
            let mut applied_ok = false;
            match frame.apply_with_menu(&mut self.body, tick) {
                Ok(applied) => {
                    applied_ok = true;
                    self.held = frame.held;
                    for click in &frame.clicks {
                        match click {
                            Click::Attack => stats
                                .attack_clicks
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                            Click::Use => stats
                                .use_clicks
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                        };
                    }
                    if applied.clicks > 0 {
                        log::debug!("clicks: {:?}", frame.clicks);
                    }
                }
                Err(error) => {
                    // A dropped frame must not leave a control held.
                    stats
                        .frames_dropped
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    log::warn!("frame dropped: {error}");
                    self.held = Held::NONE;
                }
            }
            let report = if applied_ok {
                self.interact(&frame, writer, trace).await?
            } else {
                AppliedReport::default()
            };
            if let Some(driver) = &self.driver {
                driver
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .applied(tick, &report);
            }
            stepped = self.step_physics(stats);
        }

        // Azalea only sends positions once the client has something to move
        // (`With<HasClientLoaded>`); before the first step there is nothing to
        // send.
        if stepped {
            self.send_movement(writer, trace, stats).await?;
        }

        // Last packet of every tick, after everything else.
        login::send(writer, ServerboundClientTickEnd, trace).await?;
        stats
            .tick_end_packets
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// The frame for this tick: from the external driver when there is one,
    /// else from the script runner, else idle.
    fn next_frame(&mut self, tick: u64, stats: &SessionStats) -> ControlFrame {
        if let Some(driver) = &self.driver {
            let packets = self
                .world
                .as_ref()
                .map(WorldFeed::take_packets)
                .unwrap_or_default();
            let frame = if let Some(feed) = &self.world {
                let world = feed.lock();
                driver
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .frame(&TickView {
                        tick,
                        body: &self.body,
                        world: Some(&world),
                        packets: &packets,
                    })
            } else {
                driver
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .frame(&TickView {
                        tick,
                        body: &self.body,
                        world: None,
                        packets: &packets,
                    })
            };
            stats
                .frames
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return frame;
        }
        let Some(runner) = &mut self.runner else {
            return ControlFrame::new(tick, self.body.generation(), Held::NONE);
        };
        let frame = runner.frame(tick, self.body.generation());
        stats
            .frames
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !self.script_done_logged && runner.finished() {
            self.script_done_logged = true;
            eprintln!("script finished: everything released");
        }
        frame
    }

    /// Turn the frame's clicks and menu actions into the packets a vanilla
    /// client would send, and report what the actuator used for attribution.
    async fn interact(
        &mut self,
        frame: &ControlFrame,
        writer: &mut WriteConnection<ServerboundGamePacket>,
        trace: bool,
    ) -> Result<AppliedReport, Error> {
        let Some(feed) = &self.world else {
            return Ok(AppliedReport::default());
        };
        let (packets, report) = {
            let world = feed.lock();
            crate::interact::plan(
                &mut self.actuator,
                &self.body,
                &world,
                frame,
                self.menu_state_id,
            )
        };
        for packet in packets {
            match packet {
                Outgoing::Packet(packet) => login::send(writer, *packet, trace).await?,
                Outgoing::PlayerAction {
                    action,
                    pos,
                    direction,
                    seq,
                } => {
                    login::send_player_action(writer, action, pos, direction, seq, trace).await?
                }
            }
        }
        Ok(report)
    }

    /// Step physics if the world around us is usable; returns whether we did.
    fn step_physics(&mut self, stats: &SessionStats) -> bool {
        let stepped = match &self.world {
            None => false,
            Some(feed) => {
                let world = feed.lock();
                if !world_ready(&world, self.body.position()) {
                    false
                } else {
                    self.body.step(&self.held, &*world);
                    true
                }
            }
        };

        if stepped {
            stats
                .physics_ticks
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            stats.set_position(self.body.position());
        } else if self.held.moves() {
            stats
                .refused_moves
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if !self.refusal_logged {
                self.refusal_logged = true;
                let reason = match &self.world {
                    None => "no world (start with --tee 127.0.0.1:PORT)".to_owned(),
                    Some(feed) => {
                        let world = feed.lock();
                        match world.status().reason() {
                            Some(reason) => format!("the world is invalid: {reason}"),
                            None => "the chunks around us are not loaded yet".to_owned(),
                        }
                    }
                };
                log::warn!("movement refused: {reason}");
            }
        }

        stepped
    }

    async fn send_movement(
        &mut self,
        writer: &mut WriteConnection<ServerboundGamePacket>,
        trace: bool,
        stats: &SessionStats,
    ) -> Result<(), Error> {
        let input = self.held.packet();
        if input != self.last_sent_input {
            login::send(writer, input.clone().into_variant(), trace).await?;
            self.last_sent_input = input;
            stats
                .input_packets
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        if self.body.sprinting() != self.last_sprinting {
            let Some(id) = self.own_entity_id else {
                return Ok(());
            };
            let action = if self.body.sprinting() {
                s_player_command::Action::StartSprinting
            } else {
                s_player_command::Action::StopSprinting
            };
            login::send(
                writer,
                ServerboundPlayerCommand {
                    id: MinecraftEntityId(id),
                    action,
                    data: 0,
                },
                trace,
            )
            .await?;
            self.last_sprinting = self.body.sprinting();
            stats
                .sprint_commands
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        if let Some(packet) = self.movement.packet(
            self.body.position(),
            self.body.look(),
            self.body.on_ground(),
            self.body.horizontal_collision(),
        ) {
            login::send(writer, packet, trace).await?;
            stats
                .position_packets
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }
}

/// Whether physics may run: a valid world with the chunks around the player.
///
/// Vanilla clients stop moving into chunks they do not have; reading a missing
/// chunk as air would drop the body into the void instead.
fn world_ready(world: &praxion_world::World, position: Vec3) -> bool {
    if !world.status().is_valid() || world.dimension().is_none() {
        return false;
    }
    let center = ChunkPos::from(&position);
    for x in -1..=1 {
        for z in -1..=1 {
            if !world.has_chunk(ChunkPos::new(center.x + x, center.z + z)) {
                return false;
            }
        }
    }
    true
}

/// End of one stretch of play.
enum Outcome {
    /// The server moved us back to configuration; the connection is in hand.
    /// Boxed because `GameConn` dwarfs the other variant.
    Reconfigure(Box<GameConn>),
    /// The session is over.
    Closed(Error),
}

/// One packet's worth of decision.
enum Flow {
    Continue,
    Reconfigure,
    Closed(Error),
}

/// Play, re-enter configuration when asked, and play again.
pub(crate) async fn run(
    conn: GameConn,
    ctx: &mut Context<'_>,
    world: Option<WorldFeed>,
) -> Result<(), Error> {
    let mut state = PlayState::new(
        ctx.auto_respawn,
        world,
        ctx.script.clone(),
        ctx.driver.clone(),
        Vec3::ZERO,
        LookDirection::default(),
    );
    let mut conn = conn;
    loop {
        match play_until(conn, ctx, &mut state).await {
            Outcome::Reconfigure(play_conn) => {
                conn = config::enter(play_conn.config(), ctx.trace).await?;
                // A new configuration means a new world registration; the server
                // sends the chunks again and wants a fresh player_loaded.
                state.client_loaded = false;
                state.body.respawn();
            }
            Outcome::Closed(error) => {
                ctx.stats.set_world_summary(world_summary(&state));
                return Err(error);
            }
        }
    }
}

/// One line about the world feed, for the session's log.
fn world_summary(state: &PlayState) -> String {
    let Some(feed) = &state.world else {
        return "none (no --tee)".to_owned();
    };
    let world = feed.lock();
    let warnings: u64 = world.warning_counts().values().sum();
    let reason = world
        .status()
        .reason()
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default();
    format!(
        "{:?}{reason}, {} chunks, {warnings} warnings",
        world.status(),
        world.chunk_count()
    )
}

async fn play_until(
    mut conn: GameConn,
    ctx: &mut Context<'_>,
    state: &mut PlayState,
) -> Outcome {
    let mut next_tick = Instant::now() + TICK;
    loop {
        let flow = tokio::select! {
            biased;
            command = ctx.commands.recv() => match command {
                // The owner dropped every handle.
                None => Flow::Closed(Error::LocalDisconnect),
                Some(Command::Disconnect) => {
                    let _ = conn.writer.shutdown().await;
                    Flow::Closed(Error::LocalDisconnect)
                }
            },
            packet = conn.reader.read() => match packet {
                Ok(packet) => {
                    match handle(&mut conn.writer, state, packet, ctx).await {
                        Ok(flow) => flow,
                        Err(error) => Flow::Closed(error),
                    }
                }
                Err(error) => Flow::Closed(Error::Protocol(error)),
            },
            _ = tokio::time::sleep_until(next_tick) => {
                // Fixed cadence: the deadline advances by one tick, so a slow write
                // does not stretch the tick rate. A stall longer than a tick is not
                // caught up with a burst of packets; the deadline resynchronises.
                let now = Instant::now();
                next_tick = if next_tick + TICK > now { next_tick + TICK } else { now + TICK };
                match state.tick(&mut conn.writer, ctx.trace, ctx.stats).await {
                    Ok(()) => Flow::Continue,
                    Err(error) => Flow::Closed(error),
                }
            }
        };

        match flow {
            Flow::Continue => {}
            Flow::Reconfigure => return Outcome::Reconfigure(Box::new(conn)),
            Flow::Closed(error) => return Outcome::Closed(error),
        }
    }
}

/// Answer one server packet.
async fn handle(
    writer: &mut WriteConnection<ServerboundGamePacket>,
    state: &mut PlayState,
    packet: ClientboundGamePacket,
    ctx: &Context<'_>,
) -> Result<Flow, Error> {
    if ctx.trace {
        log::debug!("<- {}", packet.name());
    }
    // The first play packet means the server is ticking with us; in vanilla this
    // packet is always `login`.
    if !state.joined {
        state.joined = true;
        let _ = ctx.events.send(Event::Joined);
    }

    match packet {
        ClientboundGamePacket::Login(p) => {
            state.own_entity_id = Some(p.player_id.0);
        }
        ClientboundGamePacket::KeepAlive(p) => {
            login::send(writer, ServerboundKeepAlive { id: p.id }, ctx.trace).await?;
        }
        ClientboundGamePacket::Ping(p) => {
            login::send(writer, ServerboundPong { id: p.id }, ctx.trace).await?;
        }
        ClientboundGamePacket::PlayerPosition(p) => {
            log::debug!("player_position {:?} relative={:?}", p.change, p.relative);
            state.body.teleport(&p.change, &p.relative);
            state
                .movement
                .teleported(state.body.position(), state.body.look());
            ctx.stats
                .teleports
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // 26.3 acknowledges with the position and direction it now has
            // (`.../packet/game/mod.rs:422-449`); no separate move packet follows.
            login::send(
                writer,
                ServerboundAcceptTeleportation {
                    id: p.id,
                    position: state.body.position(),
                    direction: state.body.look(),
                },
                ctx.trace,
            )
            .await?;
        }
        ClientboundGamePacket::PlayerRotation(p) => {
            let (yaw, pitch): (f32, f32) = state.body.look().into();
            state.body.set_look(LookDirection::new(
                apply_change(yaw, p.relative_y, p.y_rot),
                apply_change(pitch, p.relative_x, p.x_rot),
            ));
        }
        ClientboundGamePacket::ChunkBatchStart(_) => state.chunks.start(),
        ClientboundGamePacket::ChunkBatchFinished(p) => {
            let desired_chunks_per_tick = state.chunks.finished(p.batch_size);
            login::send(
                writer,
                ServerboundChunkBatchReceived {
                    desired_chunks_per_tick,
                },
                ctx.trace,
            )
            .await?;
        }
        ClientboundGamePacket::LevelChunkWithLight(p) => {
            // The vanilla client sends `player_loaded` once the chunk it stands in
            // is there (`azalea-client/src/plugins/loading.rs:33-51`). Without it the
            // server keeps us waiting for chunks and never puts us in the world.
            if !state.client_loaded && chunk_of(state.body.position()) == ChunkPos::new(p.x, p.z) {
                state.client_loaded = true;
                login::send(writer, ServerboundPlayerLoaded, ctx.trace).await?;
            }
        }
        ClientboundGamePacket::SetHealth(p) => {
            if p.health <= 0.0 {
                die(state, writer, ctx).await?;
            } else {
                state.dead = false;
            }
        }
        ClientboundGamePacket::PlayerCombatKill(p) => {
            if state.own_entity_id == Some(p.player_id.0) {
                die(state, writer, ctx).await?;
            }
        }
        ClientboundGamePacket::Respawn(_) => {
            state.dead = false;
            // The new world arrives with new chunks and a new teleport.
            state.client_loaded = false;
            state.body.respawn();
        }
        ClientboundGamePacket::CookieRequest(p) => {
            login::send(
                writer,
                ServerboundCookieResponse {
                    key: p.key,
                    payload: None,
                },
                ctx.trace,
            )
            .await?;
        }
        ClientboundGamePacket::OpenScreen(_) => {
            state.menu_state_id = 0;
        }
        ClientboundGamePacket::ContainerSetContent(p) => {
            if p.container_id != 0 {
                state.menu_state_id = p.state_id;
            }
        }
        ClientboundGamePacket::ContainerSetSlot(p) => {
            if p.container_id != 0 {
                state.menu_state_id = p.state_id;
            }
        }
        ClientboundGamePacket::ContainerClose(_) => {
            state.menu_state_id = 0;
        }
        ClientboundGamePacket::StartConfiguration(_) => {
            login::send(writer, ServerboundConfigurationAcknowledged, ctx.trace).await?;
            return Ok(Flow::Reconfigure);
        }
        ClientboundGamePacket::Disconnect(p) => {
            return Ok(Flow::Closed(Error::Disconnected(p.reason.to_string())));
        }
        _ => {}
    }
    Ok(Flow::Continue)
}

/// Death: tell the owner once, and respawn if asked to.
async fn die(
    state: &mut PlayState,
    writer: &mut WriteConnection<ServerboundGamePacket>,
    ctx: &Context<'_>,
) -> Result<(), Error> {
    if state.dead {
        return Ok(());
    }
    state.dead = true;
    let _ = ctx.events.send(Event::Died);
    if state.auto_respawn {
        login::send(
            writer,
            ServerboundClientCommand {
                action: Action::PerformRespawn,
            },
            ctx.trace,
        )
        .await?;
    }
    Ok(())
}

fn chunk_of(position: Vec3) -> ChunkPos {
    ChunkPos::from(&position)
}

fn apply_change<T: std::ops::Add<Output = T>>(base: T, relative: bool, change: T) -> T {
    if relative { base + change } else { change }
}

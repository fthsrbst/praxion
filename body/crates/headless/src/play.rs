//! The play state: answer what a vanilla client answers, tick 20 times a second,
//! and stay connected.
//!
//! Per tick this follows `azalea-client`'s order (`azalea-client/src/plugins/
//! movement.rs:57-70`, `.../tick_end.rs:18-36`): movement packets first, then
//! `client_tick_end` last. There is no input and no physics yet, so the input
//! packet and the sprint command never change and are never sent, exactly like
//! Azalea when nothing moves. Everything else is event driven: keep alive,
//! ping, chunk batches, teleports, death.

use std::time::Duration;

use azalea_core::position::ChunkPos;
use azalea_protocol::connect::WriteConnection;
use azalea_protocol::packets::ProtocolPacket;
use azalea_protocol::packets::game::{
    ClientboundGamePacket, ServerboundAcceptTeleportation, ServerboundChunkBatchReceived,
    ServerboundClientCommand, ServerboundClientTickEnd, ServerboundConfigurationAcknowledged,
    ServerboundCookieResponse, ServerboundGamePacket, ServerboundKeepAlive, ServerboundPlayerLoaded,
    ServerboundPong, s_client_command::Action,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use crate::chunks::ChunkBatchInfo;
use crate::config;
use crate::error::Error;
use crate::events::{Command, Event};
use crate::login::{self, GameConn};
use crate::movement::Movement;

/// One client tick: 20 per second, as vanilla runs it.
pub(crate) const TICK: Duration = Duration::from_millis(50);

/// What the owner can tell the session, and where it goes.
pub(crate) struct Context<'a> {
    pub commands: &'a mut UnboundedReceiver<Command>,
    pub events: &'a std::sync::mpsc::Sender<Event>,
    pub auto_respawn: bool,
    pub trace: bool,
}

/// What the session keeps between ticks: only what answers the server.
struct PlayState {
    movement: Movement,
    chunks: ChunkBatchInfo,
    /// Our own chunk has arrived, so the server may be told we are loaded.
    client_loaded: bool,
    /// Our entity id from the play login packet, used to recognise our death.
    own_entity_id: Option<i32>,
    /// We are dead and have not respawned yet; stops repeated respawn requests.
    dead: bool,
    /// The first play packet arrived, so `Event::Joined` has been sent.
    joined: bool,
    auto_respawn: bool,
}

impl PlayState {
    fn new(auto_respawn: bool) -> Self {
        Self {
            movement: Movement::default(),
            chunks: ChunkBatchInfo::default(),
            client_loaded: false,
            own_entity_id: None,
            dead: false,
            joined: false,
            auto_respawn,
        }
    }

    /// One client tick.
    async fn tick(
        &mut self,
        writer: &mut WriteConnection<ServerboundGamePacket>,
        trace: bool,
    ) -> Result<(), Error> {
        // Azalea only sends positions once the client has loaded (`With<HasClientLoaded>`
        // on `send_position`); before that the server would reject them.
        if self.client_loaded
            && let Some(packet) = self.movement.tick()
        {
            login::send(writer, packet, trace).await?;
        }
        // Last packet of every tick, after everything else.
        login::send(writer, ServerboundClientTickEnd, trace).await
    }
}

/// End of one stretch of play.
enum Outcome {
    /// The server moved us back to configuration; the connection is in hand.
    Reconfigure(GameConn),
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
pub(crate) async fn run(conn: GameConn, ctx: &mut Context<'_>) -> Result<(), Error> {
    let mut state = PlayState::new(ctx.auto_respawn);
    let mut conn = conn;
    loop {
        match play_until(conn, ctx, &mut state).await {
            Outcome::Reconfigure(play_conn) => {
                conn = config::enter(play_conn.config(), ctx.trace).await?;
                // A new configuration means a new world registration; the server sends
                // the chunks again and wants a fresh player_loaded.
                state.client_loaded = false;
            }
            Outcome::Closed(error) => return Err(error),
        }
    }
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
                Some(Command::Send(packet)) => {
                    match login::send(&mut conn.writer, packet, ctx.trace).await {
                        Ok(()) => Flow::Continue,
                        Err(error) => Flow::Closed(error),
                    }
                }
            },
            packet = conn.reader.read() => match packet {
                Ok(packet) => match handle(&mut conn.writer, state, packet, ctx).await {
                    Ok(flow) => flow,
                    Err(error) => Flow::Closed(error),
                },
                Err(error) => Flow::Closed(Error::Protocol(error)),
            },
            _ = tokio::time::sleep_until(next_tick) => {
                // Fixed cadence: the deadline advances by one tick, so a slow write
                // does not stretch the tick rate. A stall longer than a tick is not
                // caught up with a burst of packets; the deadline resynchronises.
                let now = Instant::now();
                next_tick = if next_tick + TICK > now { next_tick + TICK } else { now + TICK };
                match state.tick(&mut conn.writer, ctx.trace).await {
                    Ok(()) => Flow::Continue,
                    Err(error) => Flow::Closed(error),
                }
            }
        };

        match flow {
            Flow::Continue => {}
            Flow::Reconfigure => return Outcome::Reconfigure(conn),
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
        eprintln!("<- {}", packet.name());
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
            state.movement.teleport(&p.change, &p.relative);
            // 26.3 acknowledges with the position and direction it now has
            // (`.../packet/game/mod.rs:422-449`); no separate move packet follows.
            login::send(
                writer,
                ServerboundAcceptTeleportation {
                    id: p.id,
                    position: state.movement.position(),
                    direction: state.movement.look(),
                },
                ctx.trace,
            )
            .await?;
        }
        ClientboundGamePacket::PlayerRotation(p) => {
            state
                .movement
                .rotate(p.y_rot, p.relative_y, p.x_rot, p.relative_x);
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
            if !state.client_loaded && state.movement.chunk_pos() == ChunkPos::new(p.x, p.z) {
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

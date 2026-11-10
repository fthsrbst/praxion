//! The session against a scripted fake server.
//!
//! The fake server is a real TCP listener built out of `azalea-protocol`'s own
//! `Connection` (generics flipped) and clientbound packet encoders, so the test
//! exercises the same framing, compression and packet types a vanilla server
//! uses. It records every packet the bot sends, so the script can demand the
//! answers the protocol requires: hello, login acknowledged, known packs,
//! keep alive, pong, teleport acknowledge, chunk batch received, player loaded,
//! finish configuration, client tick end.
//!
//! The script always ends by closing the connection (or waiting for the bot to
//! close it), so the tests never race the server thread: the bot's
//! `Event::Disconnected` is the signal that every step has run.

use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use azalea_auth::game_profile::GameProfile;
use azalea_core::position::Vec3;
use azalea_entity::LookDirection;
use azalea_protocol::connect::Connection;
use azalea_protocol::packets::{
    config::{ClientboundConfigPacket, ServerboundConfigPacket},
    game::{
        ClientboundGamePacket, ServerboundGamePacket,
        c_level_chunk_with_light::ClientboundLevelChunkPacketData,
        c_light_update::ClientboundLightUpdatePacketData,
    },
    handshake::{ClientboundHandshakePacket, ServerboundHandshakePacket},
    login::{ClientboundLoginPacket, ServerboundLoginPacket},
};
use praxion_headless::{ConnectOptions, Error, Event, Session};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

/// How long one script step waits for a packet from the bot.
const PACKET_WAIT: Duration = Duration::from_secs(5);
/// How long a test waits for the next session event.
const EVENT_WAIT: Duration = Duration::from_secs(10);
/// Tick ends may arrive between any two script steps; a step skips at most this
/// many packets looking for its own (40 tick ends is two seconds).
const MAX_SKIPPED_PACKETS: usize = 60;

// ---------------------------------------------------------------------------
// Script
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Step {
    /// The next packet from the bot must be this one (see the summaries below).
    /// Tick ends and movement packets in between are skipped.
    Expect(&'static str),
    /// Send this packet to the bot.
    Send(Packet),
    /// Close the connection, as a server shutdown would.
    Close,
    /// Wait for the bot to close the connection.
    UntilClosed,
}

/// A clientbound packet the fake server can send.
#[derive(Debug)]
enum Packet {
    Hello,
    Compression(i32),
    LoginFinished,
    KnownPacks,
    ConfigKeepAlive(u64),
    ConfigPing(u32),
    FinishConfiguration,
    GameKeepAlive(u64),
    GamePing(u32),
    Teleport { id: u32, pos: (f64, f64, f64) },
    Health(f32),
    ChunkBatchStart,
    ChunkBatchFinished(u32),
    Chunk { x: i32, z: i32 },
    StartConfiguration,
}

// ---------------------------------------------------------------------------
// The server side of a connection, one state at a time
// ---------------------------------------------------------------------------

enum ServerConn {
    Handshake(Connection<ServerboundHandshakePacket, ClientboundHandshakePacket>),
    Login(Connection<ServerboundLoginPacket, ClientboundLoginPacket>),
    Config(Connection<ServerboundConfigPacket, ClientboundConfigPacket>),
    Game(Connection<ServerboundGamePacket, ClientboundGamePacket>),
}

impl std::fmt::Debug for ServerConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ServerConn::Handshake(_) => "handshake",
            ServerConn::Login(_) => "login",
            ServerConn::Config(_) => "configuration",
            ServerConn::Game(_) => "play",
        })
    }
}

impl ServerConn {
    /// Read one packet from the bot, returning this connection and a one-line
    /// summary of what arrived.
    async fn read(self) -> Result<(Self, String), String> {
        match self {
            ServerConn::Handshake(mut conn) => {
                let packet = await_packet(conn.read()).await?;
                let summary = match packet {
                    ServerboundHandshakePacket::Intention(p) => {
                        format!("intention({}, {:?})", p.protocol_version, p.intention)
                    }
                };
                Ok((ServerConn::Login(Connection::from(conn)), summary))
            }
            ServerConn::Login(mut conn) => {
                let packet = await_packet(conn.read()).await?;
                let summary = match &packet {
                    ServerboundLoginPacket::Hello(p) => format!("hello({})", p.name),
                    ServerboundLoginPacket::Key(_) => "key".to_owned(),
                    ServerboundLoginPacket::CustomQueryAnswer(_) => "custom_query_answer".to_owned(),
                    ServerboundLoginPacket::LoginAcknowledged(_) => "login_acknowledged".to_owned(),
                    ServerboundLoginPacket::CookieResponse(_) => "cookie_response".to_owned(),
                };
                // The acknowledgement is the bot's own move to configuration.
                if matches!(packet, ServerboundLoginPacket::LoginAcknowledged(_)) {
                    return Ok((ServerConn::Config(Connection::from(conn)), summary));
                }
                Ok((ServerConn::Login(conn), summary))
            }
            ServerConn::Config(mut conn) => {
                let packet = await_packet(conn.read()).await?;
                let summary = match &packet {
                    ServerboundConfigPacket::ClientInformation(_) => "client_information".to_owned(),
                    ServerboundConfigPacket::CookieResponse(_) => "cookie_response".to_owned(),
                    ServerboundConfigPacket::CustomPayload(_) => "custom_payload".to_owned(),
                    ServerboundConfigPacket::FinishConfiguration(_) => {
                        "finish_configuration".to_owned()
                    }
                    ServerboundConfigPacket::KeepAlive(p) => format!("keep_alive({})", p.id),
                    ServerboundConfigPacket::Pong(p) => format!("pong({})", p.id),
                    ServerboundConfigPacket::ResourcePack(_) => "resource_pack".to_owned(),
                    ServerboundConfigPacket::SelectKnownPacks(p) => {
                        format!("select_known_packs({})", p.known_packs.len())
                    }
                    ServerboundConfigPacket::CustomClickAction(_) => {
                        "custom_click_action".to_owned()
                    }
                    ServerboundConfigPacket::AcceptCodeOfConduct(_) => {
                        "accept_code_of_conduct".to_owned()
                    }
                };
                if matches!(packet, ServerboundConfigPacket::FinishConfiguration(_)) {
                    return Ok((ServerConn::Game(Connection::from(conn)), summary));
                }
                Ok((ServerConn::Config(conn), summary))
            }
            ServerConn::Game(mut conn) => {
                let packet = await_packet(conn.read()).await?;
                let summary = summarise_game(&packet);
                // The bot switches to configuration itself and says so; follow it.
                if matches!(packet, ServerboundGamePacket::ConfigurationAcknowledged(_)) {
                    return Ok((ServerConn::Config(Connection::from(conn)), summary));
                }
                Ok((ServerConn::Game(conn), summary))
            }
        }
    }

    /// Send one packet to the bot, returning this connection.
    async fn write(self, packet: Packet) -> Result<Self, String> {
        match (self, packet) {
            (ServerConn::Login(mut conn), Packet::Hello) => {
                let hello = azalea_protocol::packets::login::ClientboundHello {
                    server_id: "1".to_owned(),
                    public_key: vec![0; 8],
                    challenge: vec![0; 4],
                    should_authenticate: true,
                };
                write_io(conn.write(hello)).await?;
                Ok(ServerConn::Login(conn))
            }
            (ServerConn::Login(mut conn), Packet::Compression(threshold)) => {
                write_io(
                    conn.write(azalea_protocol::packets::login::ClientboundLoginCompression {
                        compression_threshold: threshold,
                    }),
                )
                .await?;
                // Only packets after this one are compressed.
                conn.set_compression_threshold(threshold);
                Ok(ServerConn::Login(conn))
            }
            (ServerConn::Login(mut conn), Packet::LoginFinished) => {
                write_io(
                    conn.write(azalea_protocol::packets::login::ClientboundLoginFinished {
                        game_profile: GameProfile::new(Uuid::nil(), "Bot1".to_owned()),
                        session_id: Uuid::nil(),
                    }),
                )
                .await?;
                Ok(ServerConn::Login(conn))
            }
            (ServerConn::Config(mut conn), Packet::KnownPacks) => {
                write_io(
                    conn.write(azalea_protocol::packets::config::ClientboundSelectKnownPacks {
                        known_packs: vec![],
                    }),
                )
                .await?;
                Ok(ServerConn::Config(conn))
            }
            (ServerConn::Config(mut conn), Packet::ConfigKeepAlive(id)) => {
                write_io(conn.write(azalea_protocol::packets::config::ClientboundKeepAlive { id }))
                    .await?;
                Ok(ServerConn::Config(conn))
            }
            (ServerConn::Config(mut conn), Packet::ConfigPing(id)) => {
                write_io(conn.write(azalea_protocol::packets::config::ClientboundPing { id })).await?;
                Ok(ServerConn::Config(conn))
            }
            (ServerConn::Config(mut conn), Packet::FinishConfiguration) => {
                write_io(conn.write(azalea_protocol::packets::config::ClientboundFinishConfiguration))
                    .await?;
                Ok(ServerConn::Config(conn))
            }
            (ServerConn::Game(mut conn), packet) => {
                let packet = game_packet(packet);
                write_io(conn.write(packet)).await?;
                Ok(ServerConn::Game(conn))
            }
            (conn, packet) => Err(format!("cannot send {packet:?} while in {conn:?}")),
        }
    }
}

fn game_packet(packet: Packet) -> ClientboundGamePacket {
    use azalea_protocol::common::movements::{
        PositionMoveRotation, RelativeMovements,
    };
    use azalea_protocol::packets::game::*;

    match packet {
        Packet::GameKeepAlive(id) => ClientboundGamePacket::KeepAlive(ClientboundKeepAlive { id }),
        Packet::GamePing(id) => ClientboundGamePacket::Ping(ClientboundPing { id }),
        Packet::Teleport { id, pos } => {
            ClientboundGamePacket::PlayerPosition(ClientboundPlayerPosition {
                id,
                change: PositionMoveRotation {
                    pos: Vec3::new(pos.0, pos.1, pos.2),
                    delta: Vec3::ZERO,
                    look_direction: LookDirection::default(),
                },
                relative: RelativeMovements::all_absolute(),
            })
        }
        Packet::Health(health) => ClientboundGamePacket::SetHealth(ClientboundSetHealth {
            health,
            food: 20,
            saturation: 5.0,
        }),
        Packet::ChunkBatchStart => ClientboundGamePacket::ChunkBatchStart(ClientboundChunkBatchStart),
        Packet::ChunkBatchFinished(batch_size) => {
            ClientboundGamePacket::ChunkBatchFinished(ClientboundChunkBatchFinished { batch_size })
        }
        Packet::Chunk { x, z } => {
            ClientboundGamePacket::LevelChunkWithLight(ClientboundLevelChunkWithLight {
                x,
                z,
                chunk_data: ClientboundLevelChunkPacketData {
                    heightmaps: vec![],
                    buffer: Arc::new(Vec::new().into_boxed_slice()),
                    block_entities: vec![],
                },
                light_data: ClientboundLightUpdatePacketData::default(),
            })
        }
        Packet::StartConfiguration => {
            ClientboundGamePacket::StartConfiguration(ClientboundStartConfiguration)
        }
        other => unreachable!("{other:?} is not a play packet"),
    }
}

fn summarise_game(packet: &ServerboundGamePacket) -> String {
    use azalea_protocol::packets::game::*;

    match packet {
        ServerboundGamePacket::KeepAlive(p) => format!("keep_alive({})", p.id),
        ServerboundGamePacket::Pong(p) => format!("pong({})", p.id),
        ServerboundGamePacket::AcceptTeleportation(p) => format!(
            "accept_teleportation({}, {}, {}, {})",
            p.id, p.position.x, p.position.y, p.position.z
        ),
        ServerboundGamePacket::ChunkBatchReceived(p) => {
            format!("chunk_batch_received({:.2})", p.desired_chunks_per_tick)
        }
        ServerboundGamePacket::PlayerLoaded(_) => "player_loaded".to_owned(),
        ServerboundGamePacket::ClientTickEnd(_) => "client_tick_end".to_owned(),
        ServerboundGamePacket::ConfigurationAcknowledged(_) => {
            "configuration_acknowledged".to_owned()
        }
        ServerboundGamePacket::CookieResponse(_) => "cookie_response".to_owned(),
        ServerboundGamePacket::ClientCommand(p) => format!("client_command({:?})", p.action),
        ServerboundGamePacket::MovePlayerPos(p) => {
            format!("move_player_pos({}, {}, {})", p.pos.x, p.pos.y, p.pos.z)
        }
        ServerboundGamePacket::MovePlayerPosRot(p) => {
            format!("move_player_pos_rot({}, {}, {})", p.pos.x, p.pos.y, p.pos.z)
        }
        ServerboundGamePacket::MovePlayerRot(_) => "move_player_rot".to_owned(),
        ServerboundGamePacket::MovePlayerStatusOnly(_) => "move_player_status_only".to_owned(),
        other => format!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The scripted server thread
// ---------------------------------------------------------------------------

fn start_server(steps: Vec<Step>) -> (SocketAddr, JoinHandle<Result<Vec<String>, String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let addr = listener.local_addr().expect("local address");
    listener
        .set_nonblocking(true)
        .expect("the listener must be non-blocking for Tokio");

    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a Tokio runtime");
        runtime.block_on(async move {
            let listener = TcpListener::from_std(listener).expect("adopt the listener");
            let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .map_err(|_| "no client connected".to_owned())?
                .map_err(|error| error.to_string())?;
            stream.set_nodelay(true).ok();
            run_script(stream, steps).await
        })
    });
    (addr, handle)
}

async fn run_script(stream: TcpStream, steps: Vec<Step>) -> Result<Vec<String>, String> {
    let mut conn = ServerConn::Handshake(Connection::wrap(stream));
    let mut recorded = Vec::new();

    for step in steps {
        match step {
            Step::Expect(expected) => {
                let mut skipped = 0;
                loop {
                    let (next, summary) = conn.read().await?;
                    conn = next;
                    let matched = summary.starts_with(expected);
                    recorded.push(summary.clone());
                    if matched {
                        break;
                    }
                    // Tick ends and movement packets legitimately interleave.
                    let harmless = summary.starts_with("client_tick_end")
                        || summary.starts_with("move_player_");
                    if !harmless || skipped >= MAX_SKIPPED_PACKETS {
                        return Err(format!("expected {expected}, got {summary}"));
                    }
                    skipped += 1;
                }
            }
            Step::Send(packet) => conn = conn.write(packet).await?,
            Step::Close => break,
            Step::UntilClosed => {
                let deadline = tokio::time::Instant::now() + PACKET_WAIT;
                loop {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        return Err("the bot never closed the connection".to_owned());
                    }
                    match tokio::time::timeout(remaining, conn.read()).await {
                        Ok(Ok((next, summary))) => {
                            conn = next;
                            recorded.push(summary);
                        }
                        // The bot closed the connection: the script is done.
                        Ok(Err(_)) => return Ok(recorded),
                        Err(_) => return Err("the bot never closed the connection".to_owned()),
                    }
                }
            }
        }
    }
    // Dropping `conn` closes the socket, like a server shutdown.
    Ok(recorded)
}

async fn await_packet<T>(
    future: impl std::future::Future<
        Output = Result<T, Box<azalea_protocol::read::ReadPacketError>>,
    >,
) -> Result<T, String> {
    match tokio::time::timeout(PACKET_WAIT, future).await {
        Ok(Ok(packet)) => Ok(packet),
        Ok(Err(error)) => Err(format!("could not read a packet: {error}")),
        Err(_) => Err("the bot sent nothing".to_owned()),
    }
}

async fn write_io(
    future: impl std::future::Future<Output = std::io::Result<()>>,
) -> Result<(), String> {
    future.await.map_err(|error| error.to_string())
}

/// The packets every login shares. `compression` is the threshold the server
/// announces, or `None` for a server that never compresses.
fn login_steps(compression: Option<i32>) -> Vec<Step> {
    let mut steps = vec![Step::Expect("intention"), Step::Expect("hello")];
    if let Some(threshold) = compression {
        steps.push(Step::Send(Packet::Compression(threshold)));
    }
    steps.extend([
        Step::Send(Packet::LoginFinished),
        Step::Expect("login_acknowledged"),
        Step::Expect("client_information"),
        Step::Send(Packet::KnownPacks),
        Step::Expect("select_known_packs"),
        Step::Send(Packet::ConfigKeepAlive(11)),
        Step::Expect("keep_alive(11)"),
        Step::Send(Packet::ConfigPing(12)),
        Step::Expect("pong(12)"),
        Step::Send(Packet::FinishConfiguration),
        Step::Expect("finish_configuration"),
    ]);
    steps
}

/// Connect, require `Joined`, then require the session to end with the reason the
/// scripted server produced.
fn run_and_wait(addr: SocketAddr, options: ConnectOptions) -> Vec<Event> {
    let session = Session::connect_with(addr, "Bot1", options).expect("the login succeeds");
    let mut events = Vec::new();
    loop {
        match session.recv_event_timeout(EVENT_WAIT) {
            Some(event) => {
                let ended = matches!(event, Event::Disconnected(_));
                events.push(event);
                if ended {
                    break;
                }
            }
            None => panic!("the session neither ended nor sent an event: {events:?}"),
        }
    }
    session.close();
    events
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn logs_in_and_answers_everything_the_protocol_asks() {
    let mut steps = login_steps(Some(256));
    steps.extend([
        // Play: the first game packet is what `Joined` means.
        Step::Send(Packet::GameKeepAlive(13)),
        Step::Expect("keep_alive(13)"),
        Step::Send(Packet::GamePing(14)),
        Step::Expect("pong(14)"),
        Step::Send(Packet::Teleport {
            id: 7,
            pos: (10.5, 64.0, -3.5),
        }),
        Step::Expect("accept_teleportation(7, 10.5, 64, -3.5)"),
        Step::Send(Packet::ChunkBatchStart),
        Step::Send(Packet::ChunkBatchFinished(4)),
        Step::Expect("chunk_batch_received"),
        // The chunk we stand in (10.5, -3.5 is chunk 0, -1) unlocks player_loaded.
        Step::Send(Packet::Chunk { x: 0, z: -1 }),
        Step::Expect("player_loaded"),
        // No `--tee`, so there is no world: the body refuses to move and the only
        // packet per tick is `client_tick_end`. Position packets need a world
        // (`movement.rs` unit-tests their timing).
        Step::Expect("client_tick_end"),
        Step::Close,
    ]);
    let (addr, server) = start_server(steps);

    let events = run_and_wait(addr, ConnectOptions::default());
    assert_eq!(events.first(), Some(&Event::Joined), "events: {events:?}");
    assert!(
        matches!(events.last(), Some(Event::Disconnected(_))),
        "events: {events:?}"
    );

    let recorded = server
        .join()
        .expect("the server thread ends")
        .expect("the script completed; a mismatch names the packet that was wrong");
    assert!(
        recorded.iter().any(|packet| packet == "player_loaded"),
        "player_loaded must have been sent: {recorded:?}"
    );
}

#[test]
fn logs_in_without_compression_too() {
    let mut steps = login_steps(None);
    steps.extend([
        Step::Send(Packet::GameKeepAlive(1)),
        Step::Expect("keep_alive(1)"),
        Step::Close,
    ]);
    let (addr, server) = start_server(steps);

    let events = run_and_wait(addr, ConnectOptions::default());
    assert_eq!(events.first(), Some(&Event::Joined));
    server
        .join()
        .expect("the server thread ends")
        .expect("the script completed");
}

#[test]
fn a_dead_player_is_reported_once_and_respawned_when_asked() {
    let mut steps = login_steps(Some(256));
    steps.extend([
        Step::Send(Packet::GameKeepAlive(20)),
        Step::Expect("keep_alive(20)"),
        Step::Send(Packet::Health(0.0)),
        Step::Expect("client_command(PerformRespawn)"),
        // The second death packet must not trigger a second request.
        Step::Send(Packet::Health(0.0)),
        Step::Expect("client_tick_end"),
        Step::Close,
    ]);
    let (addr, server) = start_server(steps);

    let events = run_and_wait(
        addr,
        ConnectOptions {
            auto_respawn: true,
            ..ConnectOptions::default()
        },
    );
    assert_eq!(events.first(), Some(&Event::Joined));
    assert_eq!(events.get(1), Some(&Event::Died { message: None }), "events: {events:?}");
    assert_eq!(
        events.iter().filter(|event| matches!(event, Event::Died { .. })).count(),
        1,
        "one death, one Died: {events:?}"
    );
    server
        .join()
        .expect("the server thread ends")
        .expect("the script completed");
}

#[test]
fn a_dead_player_stays_dead_without_auto_respawn() {
    let mut steps = login_steps(Some(256));
    steps.extend([
        Step::Send(Packet::GameKeepAlive(21)),
        Step::Expect("keep_alive(21)"),
        Step::Send(Packet::Health(0.0)),
        // No client command: the next packet is a tick end.
        Step::Expect("client_tick_end"),
        Step::Close,
    ]);
    let (addr, server) = start_server(steps);

    let events = run_and_wait(addr, ConnectOptions::default());
    assert_eq!(events.first(), Some(&Event::Joined));
    assert_eq!(events.get(1), Some(&Event::Died { message: None }), "events: {events:?}");
    server
        .join()
        .expect("the server thread ends")
        .expect("the script completed");
}

#[test]
fn going_back_to_configuration_and_out_again_keeps_the_session() {
    let mut steps = login_steps(Some(256));
    steps.extend([
        Step::Send(Packet::GameKeepAlive(30)),
        Step::Expect("keep_alive(30)"),
        Step::Send(Packet::StartConfiguration),
        Step::Expect("configuration_acknowledged"),
        // Every configuration state starts with the client information packet.
        Step::Expect("client_information"),
        Step::Send(Packet::ConfigKeepAlive(31)),
        Step::Expect("keep_alive(31)"),
        Step::Send(Packet::FinishConfiguration),
        Step::Expect("finish_configuration"),
        // Play again: the first packet is a fresh `Joined` situation, but the
        // session only reports `Joined` once.
        Step::Send(Packet::GameKeepAlive(32)),
        Step::Expect("keep_alive(32)"),
        Step::Close,
    ]);
    let (addr, server) = start_server(steps);

    let events = run_and_wait(addr, ConnectOptions::default());
    assert_eq!(events.first(), Some(&Event::Joined));
    assert_eq!(
        events.iter().filter(|event| **event == Event::Joined).count(),
        1,
        "a reconfiguration is not a second join: {events:?}"
    );
    server
        .join()
        .expect("the server thread ends")
        .expect("the script completed");
}

#[test]
fn an_online_mode_server_is_refused() {
    let steps = vec![
        Step::Expect("intention"),
        Step::Expect("hello"),
        Step::Send(Packet::Hello),
        Step::UntilClosed,
    ];
    let (addr, server) = start_server(steps);

    let error = Session::connect(addr, "Bot1").expect_err("online mode is not supported");
    assert!(
        matches!(error, Error::OnlineMode),
        "expected OnlineMode, got {error:?}"
    );
    let recorded = server
        .join()
        .expect("the server thread ends")
        .expect("the script completed");
    assert_eq!(recorded.last().map(String::as_str), Some("hello(Bot1)"));
}

#[test]
fn a_server_that_hangs_up_reports_disconnected() {
    let steps = vec![Step::Expect("intention"), Step::Expect("hello"), Step::Close];
    let (addr, server) = start_server(steps);

    let error = Session::connect(addr, "Bot1").expect_err("a closed socket is an error");
    assert!(
        matches!(error, Error::Protocol(_) | Error::Io(_)),
        "expected a connection error, got {error:?}"
    );
    let _ = server.join();
}

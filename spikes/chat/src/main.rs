//! Spike: does the 26.3 offline server accept unsigned chat from a headless
//! client, and what does a second client receive when a player speaks?
//!
//! Experiment code, not product code (`spikes/README.md`). Two offline clients
//! log in to one server; the talker (A) sends one `ServerboundChat` and one
//! `ServerboundChatCommand` (`/me`), the listener (B, the stand-in for the
//! user's own client) sends one `ServerboundChat`. Both log every chat-related
//! clientbound packet as one JSON line, plus the play `Login` packet's
//! `online_mode` and `enforces_secure_chat` flags. `t_ns` is nanoseconds since
//! the UNIX epoch, the clock every spike uses (`spikes/README.md`).
//!
//! Run it through `spikes/chat/run_chat_spike.py`, which owns the server.
//! Result: `docs/sessions/09-commands.md`, "2026-11-09 chat spike (macOS)".
//!
//! Structure: one reader task and one writer task per client. The reader never
//! arms a timer, and the writer never reads, because cancelling a half-read
//! frame in `azalea_protocol::read` loses the bytes already taken from the
//! socket (each call builds a fresh `FramedRead`, `read.rs:316-338`).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use azalea_protocol::common::client_information::ClientInformation;
use azalea_protocol::common::movements::PositionMoveRotation;
use azalea_protocol::connect::{Connection, ReadConnection, WriteConnection};
use azalea_protocol::packets::config::{
    ClientboundConfigPacket, ServerboundClientInformation, ServerboundFinishConfiguration,
    ServerboundKeepAlive as ConfigKeepAlive, ServerboundPong as ConfigPong,
    ServerboundSelectKnownPacks,
};
use azalea_protocol::packets::game::{
    ClientboundGamePacket, ServerboundAcceptTeleportation, ServerboundChat, ServerboundChatCommand,
    ServerboundClientTickEnd, ServerboundCookieResponse, ServerboundGamePacket,
    ServerboundKeepAlive, ServerboundPlayerLoaded, ServerboundPong,
};
use azalea_protocol::packets::handshake::ServerboundIntention;
use azalea_protocol::packets::login::{
    ClientboundLoginPacket, ServerboundCookieResponse as LoginCookieResponse,
    ServerboundCustomQueryAnswer, ServerboundHello, ServerboundLoginAcknowledged,
};
use azalea_protocol::packets::{ClientIntention, PROTOCOL_VERSION};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::sleep_until;

/// Bound on reaching the server's TCP port.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on the next packet while logging in and configuring. Vanilla sends these
/// at once, so a silent server is a broken one; the play state has no such bound.
/// Mirrors `body/crates/headless/src/login.rs:31-34`.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(120);
/// How long one tick takes: the tick-end cadence and the schedule resolution.
const TICK: Duration = Duration::from_millis(50);
/// Settle time after both clients are in play, before the talker speaks.
const SETTLE: Duration = Duration::from_secs(2);
/// Gap between the talker's chat line and its command.
const BETWEEN: Duration = Duration::from_secs(1);
/// Gap between the talker finishing and the listener's line.
const LISTENER_DELAY: Duration = Duration::from_secs(1);

type GameConn = Connection<ClientboundGamePacket, ServerboundGamePacket>;

fn unix_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn unix_ms() -> u64 {
    u64::try_from(unix_ns() / 1_000_000).unwrap_or(0)
}

/// One JSON line: the spike clock plus the caller's fields.
fn event(client: &str, dir: &str, packet: &str, fields: Value) -> Value {
    let mut map = match fields {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    map.insert("t_ns".to_owned(), json!(unix_ns()));
    map.insert("client".to_owned(), json!(client));
    map.insert("dir".to_owned(), json!(dir));
    map.insert("packet".to_owned(), json!(packet));
    Value::Object(map)
}

fn emit(value: Value) {
    println!("{value}");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A: speaks once both clients are in play, then releases the listener.
    Talker,
    /// B: the stand-in for the user's own client; speaks after the talker.
    Listener,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Talker => "A",
            Role::Listener => "B",
        }
    }
}

/// What the two client tasks share: who is in play, and whether the talker has
/// sent everything it had to send.
#[derive(Default)]
struct Flags {
    a_in_play: AtomicBool,
    b_in_play: AtomicBool,
    talker_done: AtomicBool,
}

impl Flags {
    fn set(&self, role: Role) {
        match role {
            Role::Talker => self.a_in_play.store(true, Ordering::SeqCst),
            Role::Listener => self.b_in_play.store(true, Ordering::SeqCst),
        }
    }

    fn both_in_play(&self) -> bool {
        self.a_in_play.load(Ordering::SeqCst) && self.b_in_play.load(Ordering::SeqCst)
    }
}

/// A packet the reader wants the writer to send, with the log line for it.
struct Out {
    label: &'static str,
    packet: ServerboundGamePacket,
    fields: Value,
}

#[derive(Debug)]
enum Action {
    Chat(String),
    Command(String),
    /// Mark the talker as finished and let the listener go.
    Release,
}

async fn write(
    writer: &mut WriteConnection<ServerboundGamePacket>,
    who: &str,
    label: &'static str,
    packet: ServerboundGamePacket,
    fields: Value,
) -> Result<(), String> {
    emit(event(who, "serverbound", label, fields));
    writer
        .write(packet)
        .await
        .map_err(|error| format!("{who}: writing {label}: {error}"))
}

/// Handshake, login and configuration; returns a connection in play state.
async fn log_in(addr: SocketAddr, who: &str) -> Result<GameConn, String> {
    let mut handshake = tokio::time::timeout(CONNECT_TIMEOUT, Connection::new(&addr))
        .await
        .map_err(|_| format!("{who}: connect timed out after {}s", CONNECT_TIMEOUT.as_secs()))?
        .map_err(|error| format!("{who}: connect: {error}"))?;
    emit(event(
        who,
        "serverbound",
        "intention",
        json!({ "protocol": PROTOCOL_VERSION, "host": addr.ip().to_string(), "port": addr.port() }),
    ));
    handshake
        .write(ServerboundIntention {
            protocol_version: PROTOCOL_VERSION,
            hostname: addr.ip().to_string(),
            port: addr.port(),
            intention: ClientIntention::Login,
        })
        .await
        .map_err(|error| format!("{who}: intention: {error}"))?;

    let mut conn = handshake.login();
    emit(event(
        who,
        "serverbound",
        "hello",
        json!({ "name": who, "profile_id": uuid::Uuid::nil().to_string() }),
    ));
    conn.write(ServerboundHello {
        name: who.to_owned(),
        // Offline mode: the server assigns the uuid, as the body does.
        profile_id: uuid::Uuid::nil(),
    })
    .await
    .map_err(|error| format!("{who}: hello: {error}"))?;

    let mut conn = loop {
        let packet = tokio::time::timeout(LOGIN_TIMEOUT, conn.read())
            .await
            .map_err(|_| format!("{who}: no login packet for {}s", LOGIN_TIMEOUT.as_secs()))?
            .map_err(|error| format!("{who}: login read: {error}"))?;
        match packet {
            ClientboundLoginPacket::Hello(_) => {
                return Err(format!("{who}: server asked for online mode"));
            }
            ClientboundLoginPacket::LoginCompression(p) => {
                conn.set_compression_threshold(p.compression_threshold);
            }
            ClientboundLoginPacket::CustomQuery(p) => {
                conn.write(ServerboundCustomQueryAnswer {
                    transaction_id: p.transaction_id,
                    data: None,
                })
                .await
                .map_err(|error| format!("{who}: custom query answer: {error}"))?;
            }
            ClientboundLoginPacket::CookieRequest(p) => {
                conn.write(LoginCookieResponse {
                    key: p.key,
                    payload: None,
                })
                .await
                .map_err(|error| format!("{who}: cookie response: {error}"))?;
            }
            ClientboundLoginPacket::LoginFinished(p) => {
                // The login uuid: what an operator filter would compare against
                // if a player chat carried it.
                emit(event(
                    who,
                    "clientbound",
                    "login_finished",
                    json!({
                        "game_profile_name": p.game_profile.name,
                        "game_profile_uuid": p.game_profile.uuid.to_string(),
                    }),
                ));
                conn.write(ServerboundLoginAcknowledged)
                    .await
                    .map_err(|error| format!("{who}: login acknowledged: {error}"))?;
                break conn.config();
            }
            ClientboundLoginPacket::LoginDisconnect(p) => {
                return Err(format!("{who}: login disconnect: {}", p.reason));
            }
        }
    };

    conn.write(ServerboundClientInformation {
        information: ClientInformation::default(),
    })
    .await
    .map_err(|error| format!("{who}: client information: {error}"))?;

    let conn = loop {
        let packet = tokio::time::timeout(LOGIN_TIMEOUT, conn.read())
            .await
            .map_err(|_| {
                format!("{who}: no configuration packet for {}s", LOGIN_TIMEOUT.as_secs())
            })?
            .map_err(|error| format!("{who}: configuration read: {error}"))?;
        match packet {
            ClientboundConfigPacket::SelectKnownPacks(_) => {
                conn.write(ServerboundSelectKnownPacks { known_packs: vec![] })
                    .await
                    .map_err(|error| format!("{who}: known packs: {error}"))?;
            }
            ClientboundConfigPacket::KeepAlive(p) => {
                conn.write(ConfigKeepAlive { id: p.id })
                    .await
                    .map_err(|error| format!("{who}: config keep alive: {error}"))?;
            }
            ClientboundConfigPacket::Ping(p) => {
                conn.write(ConfigPong { id: p.id })
                    .await
                    .map_err(|error| format!("{who}: config pong: {error}"))?;
            }
            ClientboundConfigPacket::CookieRequest(p) => {
                conn.write(azalea_protocol::packets::config::ServerboundCookieResponse {
                    key: p.key,
                    payload: None,
                })
                .await
                .map_err(|error| format!("{who}: config cookie: {error}"))?;
            }
            ClientboundConfigPacket::FinishConfiguration(_) => {
                conn.write(ServerboundFinishConfiguration)
                    .await
                    .map_err(|error| format!("{who}: finish configuration: {error}"))?;
                break conn.game();
            }
            ClientboundConfigPacket::Disconnect(p) => {
                return Err(format!("{who}: configuration disconnect: {}", p.reason));
            }
            _ => {}
        }
    };
    Ok(conn)
}

/// Read forever, answer what the server must hear, and forward the rest to the
/// writer. Never cancelled mid-frame.
async fn read_loop(
    role: Role,
    flags: Arc<Flags>,
    mut reader: ReadConnection<ClientboundGamePacket>,
    out: mpsc::UnboundedSender<Out>,
) -> Result<(), String> {
    let who = role.name();
    // The position the server last put us at, so player_loaded can name the chunk.
    let mut position = None;
    let mut loaded = false;
    loop {
        let packet = reader
            .read()
            .await
            .map_err(|error| format!("{who}: play read: {error}"))?;
        let forwarded = match packet {
            ClientboundGamePacket::Login(p) => {
                emit(event(
                    who,
                    "clientbound",
                    "login",
                    json!({
                        "player_id": p.player_id.0,
                        "online_mode": p.online_mode,
                        "enforces_secure_chat": p.enforces_secure_chat,
                    }),
                ));
                flags.set(role);
                None
            }
            ClientboundGamePacket::KeepAlive(p) => Some(Out {
                label: "keep_alive",
                packet: ServerboundGamePacket::KeepAlive(ServerboundKeepAlive { id: p.id }),
                fields: json!({ "id": p.id }),
            }),
            ClientboundGamePacket::Ping(p) => Some(Out {
                label: "pong",
                packet: ServerboundGamePacket::Pong(ServerboundPong { id: p.id }),
                fields: json!({ "id": p.id }),
            }),
            ClientboundGamePacket::PlayerPosition(p) => {
                position = Some(p.change.clone());
                Some(Out {
                    label: "accept_teleportation",
                    packet: ServerboundGamePacket::AcceptTeleportation(ServerboundAcceptTeleportation {
                        id: p.id,
                        position: p.change.pos,
                        direction: p.change.look_direction,
                    }),
                    fields: json!({ "id": p.id }),
                })
            }
            ClientboundGamePacket::LevelChunkWithLight(p) => {
                // The vanilla client sends player_loaded once the chunk it stands
                // in exists; without it the server keeps the player out of the
                // world and chat is not delivered
                // (`body/crates/headless/src/play.rs:603-611`).
                if loaded {
                    None
                } else {
                    let ours = position.as_ref().is_some_and(|change: &PositionMoveRotation| {
                        let chunk = (
                            (change.pos.x.floor() as i32) >> 4,
                            (change.pos.z.floor() as i32) >> 4,
                        );
                        chunk == (p.x >> 4, p.z >> 4)
                    });
                    if ours {
                        loaded = true;
                        Some(Out {
                            label: "player_loaded",
                            packet: ServerboundGamePacket::PlayerLoaded(ServerboundPlayerLoaded),
                            fields: json!({ "chunk_x": p.x, "chunk_z": p.z }),
                        })
                    } else {
                        None
                    }
                }
            }
            ClientboundGamePacket::PlayerChat(p) => {
                emit(event(
                    who,
                    "clientbound",
                    "player_chat",
                    json!({
                        "sender": p.sender.to_string(),
                        "index": p.index,
                        "global_index": p.global_index,
                        "signature": p.signature.is_some(),
                        "body_content": p.body.content,
                        "body_timestamp": p.body.timestamp,
                        "body_salt": p.body.salt,
                        "body_last_seen": p.body.last_seen.entries.len(),
                        "unsigned_content": p.unsigned_content.as_ref().map(ToString::to_string),
                        "filter_mask": format!("{:?}", p.filter_mask),
                        "chat_type_name": p.chat_type.name.to_string(),
                        "chat_type_target": p.chat_type.target_name.as_ref().map(ToString::to_string),
                        "chat_type_holder": format!("{:?}", p.chat_type.chat_type),
                    }),
                ));
                None
            }
            ClientboundGamePacket::DisguisedChat(p) => {
                emit(event(
                    who,
                    "clientbound",
                    "disguised_chat",
                    json!({
                        "text": p.message.to_string(),
                        "chat_type_name": p.chat_type.name.to_string(),
                        "chat_type_target": p.chat_type.target_name.as_ref().map(ToString::to_string),
                        "chat_type_holder": format!("{:?}", p.chat_type.chat_type),
                    }),
                ));
                None
            }
            ClientboundGamePacket::SystemChat(p) => {
                emit(event(
                    who,
                    "clientbound",
                    "system_chat",
                    json!({ "text": p.content.to_string(), "overlay": p.overlay }),
                ));
                None
            }
            ClientboundGamePacket::CookieRequest(p) => {
                let key = p.key.to_string();
                Some(Out {
                    label: "cookie_response",
                    packet: ServerboundGamePacket::CookieResponse(ServerboundCookieResponse {
                        key: p.key,
                        payload: None,
                    }),
                    fields: json!({ "key": key }),
                })
            }
            ClientboundGamePacket::StartConfiguration(_) => {
                return Err(format!("{who}: server restarted configuration mid-spike"));
            }
            ClientboundGamePacket::Disconnect(p) => {
                return Err(format!("{who}: play disconnect: {}", p.reason));
            }
            _ => None,
        };
        if let Some(message) = forwarded
            && out.send(message).is_err()
        {
            return Ok(());
        }
    }
}

/// Own the write half: a tick end every `TICK`, plus the scheduled lines.
async fn writer_loop(
    role: Role,
    flags: Arc<Flags>,
    mut writer: WriteConnection<ServerboundGamePacket>,
    mut out: mpsc::UnboundedReceiver<Out>,
    deadline: Instant,
) -> Result<(), String> {
    let who = role.name();
    let mut schedule: Vec<(Instant, Action)> = Vec::new();
    let mut armed = false;

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let next = now + TICK;
        tokio::select! {
            message = out.recv() => {
                let Some(message) = message else { break };
                write(&mut writer, who, message.label, message.packet, message.fields).await?;
            }
            () = sleep_until(tokio::time::Instant::from_std(next)) => {}
        }

        // A tick, as a vanilla client sends it at the end of every tick.
        writer
            .write(ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd))
            .await
            .map_err(|error| format!("{who}: tick end: {error}"))?;

        match role {
            Role::Talker => {
                if !armed && flags.both_in_play() {
                    armed = true;
                    let start = Instant::now();
                    schedule.push((
                        start + SETTLE,
                        Action::Chat("chat spike: unsigned line from A".to_owned()),
                    ));
                    schedule.push((
                        start + SETTLE + BETWEEN,
                        Action::Command("me waves at the spike".to_owned()),
                    ));
                    schedule.push((start + SETTLE + BETWEEN, Action::Release));
                }
            }
            Role::Listener => {
                if !armed && flags.talker_done.load(Ordering::SeqCst) {
                    armed = true;
                    schedule.push((
                        Instant::now() + LISTENER_DELAY,
                        Action::Chat("chat spike: line from B (the user)".to_owned()),
                    ));
                }
            }
        }

        while let Some(index) = schedule.iter().position(|(at, _)| *at <= Instant::now()) {
            let (_, action) = schedule.remove(index);
            match action {
                Action::Chat(text) => {
                    write(
                        &mut writer,
                        who,
                        "chat",
                        ServerboundGamePacket::Chat(ServerboundChat {
                            message: text.clone(),
                            timestamp: unix_ms(),
                            salt: 0,
                            signature: None,
                            last_seen_messages: Default::default(),
                        }),
                        json!({ "text": text, "signature": "none", "timestamp_ms": unix_ms() }),
                    )
                    .await?;
                }
                Action::Command(command) => {
                    write(
                        &mut writer,
                        who,
                        "chat_command",
                        ServerboundGamePacket::ChatCommand(ServerboundChatCommand {
                            command: command.clone(),
                        }),
                        json!({ "command": command }),
                    )
                    .await?;
                }
                Action::Release => {
                    flags.talker_done.store(true, Ordering::SeqCst);
                    emit(event(who, "local", "released_listener", json!({})));
                }
            }
        }
    }
    emit(event(who, "local", "deadline", json!({})));
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut addr: Option<SocketAddr> = None;
    let mut seconds: Option<f64> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => {
                addr = Some(
                    args.next()
                        .ok_or("--addr needs a value")?
                        .parse()
                        .map_err(|error| format!("bad address: {error}"))?,
                );
            }
            "--seconds" => {
                seconds = Some(
                    args.next()
                        .ok_or("--seconds needs a value")?
                        .parse::<f64>()
                        .map_err(|error| format!("bad seconds: {error}"))?,
                );
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let addr = addr.ok_or("--addr is required")?;
    let seconds = seconds.unwrap_or(20.0);
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);

    emit(event(
        "spike",
        "local",
        "start",
        json!({ "addr": addr.to_string(), "protocol": PROTOCOL_VERSION, "seconds": seconds }),
    ));

    // Sequential logins: A is fully in configuration before B starts, which
    // keeps the two JSON streams easy to read.
    let flags = Arc::new(Flags::default());
    let mut readers = Vec::new();
    let mut writers = Vec::new();
    for role in [Role::Talker, Role::Listener] {
        let who = role.name();
        let conn = log_in(addr, who).await?;
        let (reader, writer) = conn.into_split();
        let (tx, rx) = mpsc::unbounded_channel();
        readers.push(tokio::spawn(read_loop(role, flags.clone(), reader, tx)));
        writers.push(writer_loop(role, flags.clone(), writer, rx, deadline));
    }
    let mut writers = writers.into_iter();
    let (
        Some(talker_writer),
        Some(listener_writer),
    ) = (writers.next(), writers.next())
    else {
        return Err("two writer tasks expected".to_owned());
    };
    let (a, b) = tokio::join!(talker_writer, listener_writer);
    for reader in readers {
        reader.abort();
    }
    a?;
    b?;
    Ok(())
}

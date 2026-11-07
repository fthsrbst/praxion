//! Handshake and login: get from a TCP socket into the configuration state.
//!
//! Offline mode only: no encryption, no auth, no `azalea-auth`. The sequence
//! follows `azalea-protocol/src/connect.rs:347-619` and `azalea-client`'s
//! handlers (`azalea-client/src/plugins/packet/login/mod.rs`); see
//! `docs/research/demo0-azalea-headless.md` §1.

use std::fmt::Debug;
use std::net::SocketAddr;
use std::time::Duration;

use azalea_core::direction::Direction;
use azalea_core::position::BlockPos;
use azalea_protocol::connect::{Connection, ConnectionError, WriteConnection};
use azalea_protocol::packets::{
    ClientIntention, PROTOCOL_VERSION, Packet, ProtocolPacket,
    config::{ClientboundConfigPacket, ServerboundConfigPacket},
    game::{ClientboundGamePacket, ServerboundGamePacket, ServerboundPlayerAction, s_player_action},
    handshake::ServerboundIntention,
    login::{
        ClientboundLoginPacket, ServerboundCookieResponse, ServerboundCustomQueryAnswer,
        ServerboundHello, ServerboundLoginAcknowledged, ServerboundLoginPacket,
    },
};
use uuid::Uuid;

use crate::config;
use crate::error::Error;

/// Bound on reaching the server's TCP port.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on the next packet while logging in and configuring. Vanilla sends these
/// at once, so a silent server is a broken one; the play state has no such bound.
const PACKET_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) type LoginConn = Connection<ClientboundLoginPacket, ServerboundLoginPacket>;
pub(crate) type ConfigConn = Connection<ClientboundConfigPacket, ServerboundConfigPacket>;
pub(crate) type GameConn = Connection<ClientboundGamePacket, ServerboundGamePacket>;

/// The protocol number from `versions/manifest.toml`.
///
/// Azalea's own constant is the fallback, mirroring `praxion_forwarder`'s
/// `FALLBACK_PROTOCOL`: the manifest is read through
/// `praxion_manifest::Manifest::load_default`, whose path is compiled in, so the
/// fallback only matters if the repository file is missing at run time. Azalea's
/// constant and the manifest both say 777 for 26.3.
pub fn protocol_version() -> i32 {
    praxion_manifest::Manifest::load_default()
        .map(|manifest| manifest.minecraft.protocol as i32)
        .unwrap_or(PROTOCOL_VERSION)
}

/// Connect and log in; returns once the server has us in play state.
pub(crate) async fn connect(
    addr: SocketAddr,
    username: &str,
    trace: bool,
) -> Result<GameConn, Error> {
    let mut conn = match tokio::time::timeout(CONNECT_TIMEOUT, Connection::new(&addr)).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(ConnectionError::Io(error))) => return Err(Error::Io(error)),
        Err(_) => {
            return Err(Error::Disconnected(format!(
                "connecting to {addr} timed out after {}s",
                CONNECT_TIMEOUT.as_secs()
            )));
        }
    };

    send(
        &mut conn.writer,
        ServerboundIntention {
            protocol_version: protocol_version(),
            hostname: addr.ip().to_string(),
            port: addr.port(),
            intention: ClientIntention::Login,
        },
        trace,
    )
    .await?;

    login(conn.login(), username, trace).await
}

/// The login state: hello, compression, then configuration.
pub(crate) async fn login(
    mut conn: LoginConn,
    username: &str,
    trace: bool,
) -> Result<GameConn, Error> {
    send(
        &mut conn.writer,
        ServerboundHello {
            name: username.to_owned(),
            // Offline mode: the server assigns the uuid. Zero is what Azalea sends too.
            profile_id: Uuid::nil(),
        },
        trace,
    )
    .await?;

    loop {
        match read(&mut conn, trace).await? {
            ClientboundLoginPacket::Hello(_) => return Err(Error::OnlineMode),
            ClientboundLoginPacket::LoginCompression(p) => {
                // The threshold applies to both halves from here on.
                conn.set_compression_threshold(p.compression_threshold);
            }
            ClientboundLoginPacket::CustomQuery(p) => {
                // Plugins ask; a vanilla client answers "nothing", so do we.
                send(
                    &mut conn.writer,
                    ServerboundCustomQueryAnswer {
                        transaction_id: p.transaction_id,
                        data: None,
                    },
                    trace,
                )
                .await?;
            }
            ClientboundLoginPacket::CookieRequest(p) => {
                send(
                    &mut conn.writer,
                    ServerboundCookieResponse {
                        key: p.key,
                        payload: None,
                    },
                    trace,
                )
                .await?;
            }
            ClientboundLoginPacket::LoginFinished(p) => {
                if trace {
                    log::debug!(
                        "<- login_finished ({}, {})",
                        p.game_profile.name, p.game_profile.uuid
                    );
                }
                // 1.20.2 and later (26.3 included): acknowledge, then switch state.
                send(&mut conn.writer, ServerboundLoginAcknowledged, trace).await?;
                return config::enter(conn.config(), trace).await;
            }
            ClientboundLoginPacket::LoginDisconnect(p) => {
                return Err(Error::Disconnected(p.reason.to_string()));
            }
        }
    }
}

/// Write one packet, logging its name first when tracing.
pub(crate) async fn send<W>(
    writer: &mut WriteConnection<W>,
    packet: impl Packet<W>,
    trace: bool,
) -> Result<(), Error>
where
    W: ProtocolPacket + Debug,
{
    let packet = packet.into_variant();
    if trace {
        log::debug!("-> {}", packet.name());
    }
    writer.write(packet).await?;
    Ok(())
}

/// Write a `ServerboundPlayerAction` with the server's 26.3 action ordinal.
///
/// The pinned azalea enum has seven variants and no
/// `CHANGE_DESTROY_DIRECTION`, so its ordinals for abort, stop and
/// release-use-item are one lower than the 26.3 server's
/// (`ServerboundPlayerActionPacket$Action`: START 0, CHANGE_DIRECTION 1, ABORT 2,
/// STOP 3, DROP_ALL 4, DROP 5, RELEASE 6, SWAP 7, STAB 8). Serialising a normal
/// packet and patching the action byte keeps the rest of the codec (position,
/// direction, sequence) exactly azalea's.
pub(crate) async fn send_player_action<W>(
    writer: &mut WriteConnection<W>,
    action: u8,
    pos: BlockPos,
    direction: Direction,
    seq: u32,
    trace: bool,
) -> Result<(), Error>
where
    W: ProtocolPacket + Debug,
{
    let packet = ServerboundGamePacket::PlayerAction(ServerboundPlayerAction {
        action: s_player_action::Action::StartDestroyBlock,
        pos,
        direction,
        seq,
    });
    let mut bytes = azalea_protocol::write::serialize_packet(&packet)
        .map_err(|error| Error::Io(std::io::Error::other(error.to_string())))?;
    let id_len = varint_len(&bytes);
    if let Some(byte) = bytes.get_mut(id_len) {
        *byte = action;
    }
    if trace {
        log::debug!("-> player_action {action} {pos:?} {direction:?} seq {seq}");
    }
    writer.raw.write(&bytes).await.map_err(Error::Io)
}

/// The length of the first VarInt in `bytes`.
fn varint_len(bytes: &[u8]) -> usize {
    let mut len = 0;
    for byte in bytes {
        len += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }
    len
}

/// Read one packet with the login/configuration silence bound, logging its name.
pub(crate) async fn read<R, W>(conn: &mut Connection<R, W>, trace: bool) -> Result<R, Error>
where
    R: ProtocolPacket + Debug,
    W: ProtocolPacket + Debug,
{
    let packet = match tokio::time::timeout(PACKET_TIMEOUT, conn.read()).await {
        Ok(packet) => packet?,
        Err(_) => {
            return Err(Error::Disconnected(format!(
                "no packet from the server for {}s",
                PACKET_TIMEOUT.as_secs()
            )));
        }
    };
    if trace {
        log::debug!("<- {}", packet.name());
    }
    Ok(packet)
}

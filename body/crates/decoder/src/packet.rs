//! Protocol states, directions and the typed packet enum.
//!
//! Every packet is kept as the Azalea enum for its state and direction. The
//! wrapper only answers the three questions the rest of the body asks of a
//! packet: which state it belongs to, which direction it travels in and what it
//! is called.

use std::fmt;

use azalea_protocol::packets::{
    ProtocolPacket,
    config::{ClientboundConfigPacket, ServerboundConfigPacket},
    game::{ClientboundGamePacket, ServerboundGamePacket},
    handshake::ServerboundHandshakePacket,
    login::{ClientboundLoginPacket, ServerboundLoginPacket},
    status::{ClientboundStatusPacket, ServerboundStatusPacket},
};

/// The protocol state a frame was decoded in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum State {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Handshake => "handshake",
            State::Status => "status",
            State::Login => "login",
            State::Configuration => "configuration",
            State::Play => "play",
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The direction a frame travelled in. `Opened`, `Closed` and `Gap` records are
/// connection events, not frames, and are reported as [`crate::Event`]s.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Dir {
    Serverbound,
    Clientbound,
}

impl Dir {
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::Serverbound => "serverbound",
            Dir::Clientbound => "clientbound",
        }
    }
}

impl fmt::Display for Dir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One decoded packet, already read by Azalea.
///
/// The variants have very different sizes because they wrap Azalea's packet
/// enums; boxing them would make every match in the workspace dereference and
/// buy little on a per-frame decode path.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum Packet {
    HandshakeServerbound(ServerboundHandshakePacket),
    StatusServerbound(ServerboundStatusPacket),
    StatusClientbound(ClientboundStatusPacket),
    LoginServerbound(ServerboundLoginPacket),
    LoginClientbound(ClientboundLoginPacket),
    ConfigurationServerbound(ServerboundConfigPacket),
    ConfigurationClientbound(ClientboundConfigPacket),
    PlayServerbound(ServerboundGamePacket),
    PlayClientbound(ClientboundGamePacket),
}

impl Packet {
    /// Mojang's packet name without the `minecraft:` prefix.
    pub fn name(&self) -> &'static str {
        match self {
            Packet::HandshakeServerbound(p) => p.name(),
            Packet::StatusServerbound(p) => p.name(),
            Packet::StatusClientbound(p) => p.name(),
            Packet::LoginServerbound(p) => p.name(),
            Packet::LoginClientbound(p) => p.name(),
            Packet::ConfigurationServerbound(p) => p.name(),
            Packet::ConfigurationClientbound(p) => p.name(),
            Packet::PlayServerbound(p) => p.name(),
            Packet::PlayClientbound(p) => p.name(),
        }
    }

    /// The packet id within its state and direction.
    pub fn id(&self) -> u32 {
        match self {
            Packet::HandshakeServerbound(p) => p.id(),
            Packet::StatusServerbound(p) => p.id(),
            Packet::StatusClientbound(p) => p.id(),
            Packet::LoginServerbound(p) => p.id(),
            Packet::LoginClientbound(p) => p.id(),
            Packet::ConfigurationServerbound(p) => p.id(),
            Packet::ConfigurationClientbound(p) => p.id(),
            Packet::PlayServerbound(p) => p.id(),
            Packet::PlayClientbound(p) => p.id(),
        }
    }

    pub fn state(&self) -> State {
        match self {
            Packet::HandshakeServerbound(_) => State::Handshake,
            Packet::StatusServerbound(_) | Packet::StatusClientbound(_) => State::Status,
            Packet::LoginServerbound(_) | Packet::LoginClientbound(_) => State::Login,
            Packet::ConfigurationServerbound(_) | Packet::ConfigurationClientbound(_) => {
                State::Configuration
            }
            Packet::PlayServerbound(_) | Packet::PlayClientbound(_) => State::Play,
        }
    }

    pub fn dir(&self) -> Dir {
        match self {
            Packet::HandshakeServerbound(_)
            | Packet::StatusServerbound(_)
            | Packet::LoginServerbound(_)
            | Packet::ConfigurationServerbound(_)
            | Packet::PlayServerbound(_) => Dir::Serverbound,
            Packet::StatusClientbound(_)
            | Packet::LoginClientbound(_)
            | Packet::ConfigurationClientbound(_)
            | Packet::PlayClientbound(_) => Dir::Clientbound,
        }
    }
}

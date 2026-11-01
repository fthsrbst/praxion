//! Turn one decompressed frame into the Azalea packet enum for its state.

use std::io::Cursor;

use azalea_buf::AzBufVar;
use azalea_protocol::packets::{
    ProtocolPacket,
    config::{ClientboundConfigPacket, ServerboundConfigPacket},
    game::{ClientboundGamePacket, ServerboundGamePacket},
    handshake::ServerboundHandshakePacket,
    login::{ClientboundLoginPacket, ServerboundLoginPacket},
    status::{ClientboundStatusPacket, ServerboundStatusPacket},
};

use crate::packet::{Dir, Packet, State};

/// A packet Azalea could not read. `id` is `u32::MAX` when even the id was
/// unreadable; it is the key of the per-connection failure counter.
#[derive(Clone, Debug)]
pub struct DecodeError {
    pub id: u32,
    pub message: String,
    pub bytes_left: usize,
}

/// Decode `raw` (packet id first, fields after) and return the packet with the
/// number of bytes it consumed.
pub fn decode(state: State, dir: Dir, raw: &[u8]) -> Result<(Packet, usize), DecodeError> {
    let mut cursor = Cursor::new(raw);
    let id = match u32::azalea_read_var(&mut cursor) {
        Ok(id) => id,
        Err(e) => {
            return Err(DecodeError {
                id: u32::MAX,
                message: format!("couldn't read the packet id: {e}"),
                bytes_left: raw.len(),
            });
        }
    };
    let id_bytes = cursor.position() as usize;

    macro_rules! read {
        ($ty:ty) => {
            match <$ty>::read(id, &mut cursor) {
                Ok(packet) => packet,
                Err(e) => {
                    return Err(DecodeError {
                        id,
                        message: e.to_string(),
                        bytes_left: raw.len().saturating_sub(cursor.position() as usize),
                    });
                }
            }
        };
    }

    let packet = match (state, dir) {
        (State::Handshake, Dir::Serverbound) => {
            Packet::HandshakeServerbound(read!(ServerboundHandshakePacket))
        }
        (State::Handshake, Dir::Clientbound) => {
            return Err(DecodeError {
                id,
                message: "the server never sends a clientbound handshake packet".to_owned(),
                bytes_left: raw.len().saturating_sub(id_bytes),
            });
        }
        (State::Status, Dir::Serverbound) => {
            Packet::StatusServerbound(read!(ServerboundStatusPacket))
        }
        (State::Status, Dir::Clientbound) => {
            Packet::StatusClientbound(read!(ClientboundStatusPacket))
        }
        (State::Login, Dir::Serverbound) => {
            Packet::LoginServerbound(read!(ServerboundLoginPacket))
        }
        (State::Login, Dir::Clientbound) => {
            Packet::LoginClientbound(read!(ClientboundLoginPacket))
        }
        (State::Configuration, Dir::Serverbound) => {
            Packet::ConfigurationServerbound(read!(ServerboundConfigPacket))
        }
        (State::Configuration, Dir::Clientbound) => {
            Packet::ConfigurationClientbound(read!(ClientboundConfigPacket))
        }
        (State::Play, Dir::Serverbound) => Packet::PlayServerbound(read!(ServerboundGamePacket)),
        (State::Play, Dir::Clientbound) => Packet::PlayClientbound(read!(ClientboundGamePacket)),
    };
    Ok((packet, cursor.position() as usize))
}

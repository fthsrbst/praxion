//! The two directions of the session's channel: events out, commands in.

use azalea_protocol::packets::game::ServerboundGamePacket;

/// Something the session tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The server has us in play state: it sent the first game packet, so it is
    /// ticking with us and (in vanilla) our player entity exists.
    Joined,
    /// The session ended. The string is the reason: a server kick, an I/O error
    /// or a local disconnect.
    Disconnected(String),
    /// Our player died. With `auto_respawn` the session has already asked to
    /// respawn.
    Died,
}

/// Something the owner tells the session.
#[derive(Debug)]
pub(crate) enum Command {
    /// Write this packet to the server, in play state.
    Send(ServerboundGamePacket),
    /// Close the connection and end the session thread.
    Disconnect,
}

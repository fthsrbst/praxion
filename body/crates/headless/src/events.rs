//! The two directions of the session's channel: events out, commands in.

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
    /// Close the connection and end the session thread.
    Disconnect,
}

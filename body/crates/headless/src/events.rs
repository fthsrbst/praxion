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
    ///
    /// `message` is the death message from this session's own
    /// `ClientboundPlayerCombatKill`, rendered as plain English text: the
    /// server sends the vanilla translation (`death.attack.arrow` with the
    /// victim and killer as arguments) and azalea renders it with its bundled
    /// `en_us` table, so it reads "Praxion was shot by Skeleton". `None` when
    /// a zero-health update reported the death before the kill packet, or the
    /// server sends an empty message (`showDeathMessages` off).
    Died { message: Option<String> },
}

/// Something the owner tells the session.
#[derive(Debug)]
pub(crate) enum Command {
    /// Close the connection and end the session thread.
    Disconnect,
}
